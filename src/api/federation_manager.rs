//! Runtime manager for the OUTBOUND federation peer set.
//!
//! One [`FederationPeerManager`] is shared (as an `Arc`) between the API
//! [`ServerHandle`](crate::api::ServerHandle), the app's `reload-config` path,
//! and a lightweight saved-profile watcher. It owns:
//! - the coordinator source: explicit peers plus last-good saved profiles and
//!   immutable-profile trust policy;
//! - one poll thread, one event relay thread (see
//!   [`federation_relay`](crate::api::federation_relay)) and one proxy route per
//!   resolved outbound peer;
//! - retiring poll joins, kept off the single-threaded app path.
//!
//! `reconcile_config` and the watcher serialize source changes under the
//! coordinator lock, then reconcile the complete desired set. Explicit TCP or
//! non-profile peers remain available when coordinator mode is off. Saved SSH
//! profiles are included only when coordinator mode and per-profile policy are
//! both present.
//!
//! ## Locking model (no deadlock possible)
//! Source-driven updates take **C** (`coordinator`) before **H** (`handles`).
//! Reconcile then takes **S** (`store`) only briefly per eviction, **P**
//! (`reaper`) after releasing S, and finally **R** (`registry`): **C → H → S /
//! P → R**. Poll and relay threads take S only (the relay inside a route
//! lifecycle read, which is never taken while S is held); proxy threads take
//! R only; bridge workers take none of these locks. Shutdown joins the catalog
//! watcher before draining H, so it never waits for C while holding a
//! downstream lock.
//!
//! ## Default-off byte-identical
//! With no peer configured (or none with an `endpoint`), reconcile spawns zero
//! threads and swaps in an empty registry, so the outbound router never matches
//! and the local `agent.list` path is unchanged. `reconcile(&[])` is a clean
//! no-op / full teardown.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::{debug, info};

use crate::api::client::{ApiClient, ConnectionTarget};
use crate::api::federation_relay::{PeerRelay, RelayTiming};
use crate::api::federation_store::FederationStore;
use crate::api::peer_channel::PeerChannel;
use crate::api::schema::{CoordinatorMachineStatus, MachineEndpointStatus, SavedMachineState};
use crate::api::server::{read_peer_token, run_federation_peer_poll};
use crate::api::EventHub;
use crate::config::{FederationConfig, FederationPeer, FederationSavedMachinePolicy};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerPresentation {
    pub(crate) profile_id: Option<String>,
    pub(crate) label: String,
}

impl PeerPresentation {
    fn from_peer(peer: &FederationPeer) -> Self {
        Self {
            profile_id: peer.profile_id.clone(),
            label: peer
                .display_label
                .clone()
                .unwrap_or_else(|| peer.alias.clone()),
        }
    }
}

/// Owns the persistent SSH process behind a saved peer's local bridge. A
/// monitor replaces the process in place when it reports a terminal failure.
struct SavedBridgeSupervisor {
    join: Option<JoinHandle<()>>,
    cancellation: Arc<AtomicBool>,
}

impl SavedBridgeSupervisor {
    fn retire(mut self) -> JoinHandle<()> {
        self.cancellation.store(true, Ordering::Release);
        retire_poll(self.join.take().expect("bridge supervisor join handle"))
    }
}

impl Drop for SavedBridgeSupervisor {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            drop(retire_poll(join));
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerRouteStamp {
    pub(crate) generation: u64,
    pub(crate) remote_boot_id: Option<String>,
}

/// One resolved outbound route plus the generation/boot fence shared by every
/// request and stream using it.
#[derive(Clone, Debug)]
pub(crate) struct PeerRoute {
    target: ConnectionTarget,
    state: Arc<PeerRouteState>,
    /// Live label/profile presentation shared with the poll thread, used to
    /// stamp routed replies (`workspace.get`, `tab.*`) the same way polled
    /// entries are. `None` for routes without a poll thread.
    presentation: Option<Arc<RwLock<PeerPresentation>>>,
    /// Persistent `api-bridge --multi` channel for an SSH-bridged peer's
    /// one-shot requests; `None` for direct targets.
    peer_channel: Option<Arc<PeerChannel>>,
}

#[derive(Debug)]
struct PeerRouteState {
    generation: AtomicU64,
    remote_boot_id: RwLock<Option<String>>,
    lifecycle: RwLock<()>,
    retired: AtomicBool,
    generation_clock: Arc<AtomicU64>,
    identity_validated: AtomicBool,
    identity_validation_required: bool,
}
impl PeerRoute {
    pub(crate) fn new(
        target: ConnectionTarget,
        generation_clock: Arc<AtomicU64>,
        requires_identity_validation: bool,
    ) -> Self {
        let generation = generation_clock.fetch_add(1, Ordering::AcqRel) + 1;
        Self {
            target,
            state: Arc::new(PeerRouteState {
                generation: AtomicU64::new(generation),
                remote_boot_id: RwLock::new(None),
                lifecycle: RwLock::new(()),
                retired: AtomicBool::new(false),
                generation_clock,
                identity_validated: AtomicBool::new(!requires_identity_validation),
                identity_validation_required: requires_identity_validation,
            }),
            presentation: None,
            peer_channel: None,
        }
    }

    /// Attach the live presentation the poll thread also reads, so routed
    /// replies carry the peer's current label and saved-profile id.
    pub(crate) fn with_presentation(mut self, presentation: Arc<RwLock<PeerPresentation>>) -> Self {
        self.presentation = Some(presentation);
        self
    }

    /// Carry one-shot requests over `channel` while the peer advertises
    /// `api_bridge_multi`; the route's retirement closes it.
    pub(crate) fn with_peer_channel(mut self, channel: Arc<PeerChannel>) -> Self {
        self.peer_channel = Some(channel);
        self
    }

    pub(crate) fn peer_channel(&self) -> Option<&Arc<PeerChannel>> {
        self.peer_channel.as_ref()
    }

    /// A client for this route: one-shot requests use the peer channel when
    /// it is usable, everything else connects to the route's target.
    pub(crate) fn client(&self) -> ApiClient {
        ApiClient::for_target(self.target.clone()).with_peer_channel(self.peer_channel.clone())
    }

    /// Record whether the peer's latest poll advertised `api_bridge_multi`.
    pub(crate) fn observe_api_bridge_multi(&self, advertised: bool) {
        if let Some(channel) = &self.peer_channel {
            channel.set_advertised(advertised);
        }
    }

    /// The peer's current presentation, or the bare alias as its label when the
    /// route has none attached.
    pub(crate) fn presentation(&self, alias: &str) -> PeerPresentation {
        match &self.presentation {
            Some(presentation) => presentation
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            None => PeerPresentation {
                profile_id: None,
                label: alias.to_owned(),
            },
        }
    }

    pub(crate) fn target(&self) -> &ConnectionTarget {
        &self.target
    }

    pub(crate) fn identity_validated(&self) -> bool {
        self.state.identity_validated.load(Ordering::Acquire)
    }

    pub(crate) fn set_identity_validated(&self, validated: bool) {
        self.state.identity_validated.store(
            validated || !self.state.identity_validation_required,
            Ordering::Release,
        );
    }

    fn stamp_unlocked(&self) -> PeerRouteStamp {
        PeerRouteStamp {
            generation: self.state.generation.load(Ordering::Acquire),
            remote_boot_id: self
                .state
                .remote_boot_id
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        }
    }

    pub(crate) fn stamp(&self) -> PeerRouteStamp {
        let _lifecycle = self
            .state
            .lifecycle
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.stamp_unlocked()
    }

    pub(crate) fn is_current(&self, stamp: &PeerRouteStamp) -> bool {
        let _lifecycle = self
            .state
            .lifecycle
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !self.state.retired.load(Ordering::Acquire) && self.stamp_unlocked() == *stamp
    }

    /// Linearize an externally visible write against route retirement and
    /// remote-boot changes. The transition waits until `write` returns.
    pub(crate) fn with_current<T>(
        &self,
        stamp: &PeerRouteStamp,
        write: impl FnOnce() -> T,
    ) -> Option<T> {
        let _lifecycle = self
            .state
            .lifecycle
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (!self.state.retired.load(Ordering::Acquire) && self.stamp_unlocked() == *stamp).then(write)
    }

    pub(crate) fn observe_remote_boot(&self, remote_boot_id: Option<&str>) {
        let Some(remote_boot_id) = remote_boot_id else {
            return;
        };
        let _lifecycle = self
            .state
            .lifecycle
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut current = self
            .state
            .remote_boot_id
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.as_deref() == Some(remote_boot_id) {
            return;
        }
        if current.is_some() {
            let generation = self.state.generation_clock.fetch_add(1, Ordering::AcqRel) + 1;
            self.state.generation.store(generation, Ordering::Release);
        }
        *current = Some(remote_boot_id.to_owned());
    }

    #[cfg(test)]
    pub(crate) fn for_test(target: ConnectionTarget) -> Self {
        Self::new(target, Arc::new(AtomicU64::new(0)), false)
    }

    #[cfg(test)]
    pub(crate) fn for_test_unvalidated(target: ConnectionTarget) -> Self {
        Self::new(target, Arc::new(AtomicU64::new(0)), true)
    }

    #[cfg(test)]
    pub(crate) fn force_remote_boot_for_test(&self, remote_boot_id: &str) {
        self.observe_remote_boot(Some(remote_boot_id));
    }

    pub(crate) fn retire(&self) {
        {
            let _lifecycle = self
                .state
                .lifecycle
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.state.retired.store(true, Ordering::Release);
        }
        if let Some(channel) = &self.peer_channel {
            channel.close();
        }
    }
}

/// A single running outbound poll thread plus the handle to stop and join it.
struct PeerHandle {
    /// Per-peer cancellation flag shared by the poller and optional bridge supervisor.
    stop: Arc<AtomicBool>,
    /// Join handle for the poll thread.
    join: JoinHandle<()>,
    /// The peer's `endpoint` this thread was spawned for, for `spec_differs`.
    endpoint: String,
    /// The peer's `token_file` this thread was spawned for, for `spec_differs`.
    token_file: Option<String>,
    /// Resolved token contents used by the live TCP route.
    token: Option<String>,
    /// The peer's expected install identity this thread was spawned with.
    expected_node_id: Option<String>,
    /// Saved-machine profile this SSH bridge was spawned for.
    profile_id: Option<String>,
    /// Named remote session this SSH bridge was spawned for.
    remote_session: Option<String>,
    /// Shared route used by both polling and target proxying.
    route: PeerRoute,
    /// Mutable label/profile presentation shared with the poll thread. Label-only
    /// changes update this cell without reconnecting the transport.
    presentation: Arc<RwLock<PeerPresentation>>,
    /// Pinned saved-peer fields a Gram reverse gateway needs, if any.
    #[cfg(unix)]
    gram_spec: Option<crate::api::gram_gateway::GramGatewaySpec>,
    /// Retrying Gram reverse gateway, present while relay consent holds.
    #[cfg(unix)]
    gram_relay: Option<crate::api::gram_gateway::GramRelaySupervisor>,
    /// Keeps the saved peer's persistent SSH bridge supervised and alive.
    _ssh_bridge: Option<SavedBridgeSupervisor>,
    #[cfg(unix)]
    _reverse_gateway: Option<crate::api::reverse_agents::ReverseGateway>,
    reverse_grants: Vec<crate::config::FederationAgentGrant>,
}

#[cfg(unix)]
struct GramGatewayHooks {
    spawner: Option<Arc<dyn crate::api::gram_gateway::GramGatewaySpawner>>,
    clock: Arc<dyn crate::api::gram_gateway::GatewayClock>,
    consent: crate::api::gram_gateway::GramConsent,
}

#[derive(Clone, Default)]
struct CoordinatorSource {
    enabled: bool,
    explicit: Vec<FederationPeer>,
    policies: BTreeMap<crate::client::endpoint::ProfileId, FederationSavedMachinePolicy>,
    profiles: Vec<crate::client::endpoint::SavedSshEndpoint>,
}

/// Manages the outbound federation peer set at runtime: spawns a poll thread and
/// a proxy route per peer, and reconciles that set against desired config on
/// `reload-config`. See the module docs for the locking model.
pub struct FederationPeerManager {
    /// Serializes full reconciliation while allowing the handle lock to be
    /// released during bounded bridge retirement.
    reconcile_lock: Mutex<()>,
    /// One running poll thread per outbound peer, keyed by alias.
    handles: Mutex<HashMap<String, PeerHandle>>,
    /// The outbound proxy registry, snapshot-swapped on reconcile.
    registry: RwLock<Arc<HashMap<String, PeerRoute>>>,
    /// Joins of retiring threads, joined only once `is_finished()`.
    reaper: Mutex<Vec<JoinHandle<()>>>,
    /// Shared cache the poll threads write and `agent.list` reads.
    store: Arc<Mutex<FederationStore>>,
    /// The server's event hub, which each peer's relay thread feeds.
    event_hub: EventHub,
    /// Starts Gram reverse gateway attempts; set once the API sender exists.
    #[cfg(unix)]
    gram_spawner: Mutex<Option<Arc<dyn crate::api::gram_gateway::GramGatewaySpawner>>>,
    #[cfg(unix)]
    gram_clock: Arc<dyn crate::api::gram_gateway::GatewayClock>,
    #[cfg(unix)]
    gram_consent: crate::api::gram_gateway::GramConsent,
    /// Global daemon-running flag; poll threads also observe it for shutdown.
    running: Arc<AtomicBool>,
    /// Monotonic source for transport and remote-boot generations.
    generation_clock: Arc<AtomicU64>,
    /// Coordinator config plus the last successfully loaded saved profiles.
    coordinator: Mutex<CoordinatorSource>,
    /// Whether the last reconcile dropped federation because the active named
    /// session is not in `federation.named_sessions`; logs the skip only once.
    session_suppressed: AtomicBool,
    /// Stops the saved-profile catalog watcher independently of the daemon flag.
    catalog_stop: Arc<AtomicBool>,
    /// Catalog watcher that detects CLI add/enable/disable/remove/rename writes.
    catalog_watcher: Mutex<Option<JoinHandle<()>>>,
}

impl FederationPeerManager {
    /// Build an empty manager sharing `store`, the server's `event_hub` and the
    /// global `running` flag. A lightweight watcher observes saved-profile
    /// catalog changes; it is inert until coordinator mode is enabled by
    /// [`Self::reconcile_config`].
    pub fn new(
        store: Arc<Mutex<FederationStore>>,
        event_hub: EventHub,
        running: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Self::build(
            store,
            event_hub,
            running,
            #[cfg(unix)]
            GramGatewayHooks {
                spawner: None,
                clock: Arc::new(crate::api::gram_gateway::SystemClock),
                consent: Arc::new(|alias: &str| crate::api::gram_relay::policy().allows(alias)),
            },
        )
    }

    fn build(
        store: Arc<Mutex<FederationStore>>,
        event_hub: EventHub,
        running: Arc<AtomicBool>,
        #[cfg(unix)] gram: GramGatewayHooks,
    ) -> Arc<Self> {
        let catalog_stop = Arc::new(AtomicBool::new(false));
        let manager = Arc::new(Self {
            reconcile_lock: Mutex::new(()),
            handles: Mutex::new(HashMap::new()),
            registry: RwLock::new(Arc::new(HashMap::new())),
            reaper: Mutex::new(Vec::new()),
            store,
            event_hub,
            #[cfg(unix)]
            gram_spawner: Mutex::new(gram.spawner),
            #[cfg(unix)]
            gram_clock: gram.clock,
            #[cfg(unix)]
            gram_consent: gram.consent,
            running,
            generation_clock: Arc::new(AtomicU64::new(0)),
            coordinator: Mutex::new(CoordinatorSource::default()),
            session_suppressed: AtomicBool::new(false),
            catalog_stop: Arc::clone(&catalog_stop),
            catalog_watcher: Mutex::new(None),
        });
        let weak = Arc::downgrade(&manager);
        let watcher = std::thread::spawn(move || watch_saved_profiles(weak, catalog_stop));
        *manager
            .catalog_watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(watcher);
        manager
    }
    #[cfg(unix)]
    pub(crate) fn set_gram_api_sender(&self, tx: crate::api::ApiRequestSender) {
        *self
            .gram_spawner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(
            crate::api::reverse::SshGramGatewaySpawner::new(tx, Arc::clone(&self.running)),
        ));
    }

    fn gram_relay_consented(&self, alias: &str) -> bool {
        #[cfg(unix)]
        {
            (self.gram_consent)(alias)
        }
        #[cfg(not(unix))]
        {
            let _ = alias;
            false
        }
    }

    /// Manager with injected Gram gateway spawner, clock, and consent.
    #[cfg(all(test, unix))]
    pub(crate) fn with_gram_hooks(
        store: Arc<Mutex<FederationStore>>,
        running: Arc<AtomicBool>,
        spawner: Arc<dyn crate::api::gram_gateway::GramGatewaySpawner>,
        clock: Arc<dyn crate::api::gram_gateway::GatewayClock>,
        consent: crate::api::gram_gateway::GramConsent,
    ) -> Arc<Self> {
        Self::build(
            store,
            EventHub::default(),
            running,
            GramGatewayHooks {
                spawner: Some(spawner),
                clock,
                consent,
            },
        )
    }

    /// Gateway state for each consented peer; `off` when it has no live gateway.
    pub(crate) fn gram_relay_peer_statuses(
        &self,
        aliases: &[String],
    ) -> Vec<crate::api::schema::GramRelayPeerStatus> {
        let mut states = self.gram_relay_states();
        aliases
            .iter()
            .map(|alias| crate::api::schema::GramRelayPeerStatus {
                alias: alias.clone(),
                gateway: states
                    .remove(alias)
                    .unwrap_or(crate::api::schema::GramGatewayState::Off),
            })
            .collect()
    }

    /// Per-alias Gram reverse gateway state, snapshotted under the handle lock
    /// alone so status never nests it inside the store lock.
    fn gram_relay_states(&self) -> HashMap<String, crate::api::schema::GramGatewayState> {
        #[cfg(unix)]
        {
            self.handles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .filter_map(|(alias, handle)| {
                    Some((alias.clone(), handle.gram_relay.as_ref()?.state()))
                })
                .collect()
        }
        #[cfg(not(unix))]
        {
            HashMap::new()
        }
    }

    /// Start a gateway supervisor for every consented pinned saved peer and
    /// detach supervisors whose consent was withdrawn. Detached supervisors
    /// are returned so the caller drops (joins) them outside the handle lock.
    #[cfg(unix)]
    fn sync_gram_relays(
        &self,
        handles: &mut HashMap<String, PeerHandle>,
    ) -> Vec<crate::api::gram_gateway::GramRelaySupervisor> {
        let spawner = self
            .gram_spawner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut detached = Vec::new();
        for (alias, handle) in handles.iter_mut() {
            let consented = (self.gram_consent)(alias);
            if !consented {
                if let Some(supervisor) = handle.gram_relay.take() {
                    info!(alias = %alias, "Gram reverse gateway stopped: consent withdrawn");
                    detached.push(supervisor);
                }
                continue;
            }
            if handle.gram_relay.is_some() {
                continue;
            }
            let Some(spec) = handle.gram_spec.clone() else {
                continue;
            };
            let Some(spawner) = spawner.clone() else {
                tracing::warn!(alias = %alias, "Gram reverse gateway requires the API sender");
                continue;
            };
            handle.gram_relay = Some(crate::api::gram_gateway::GramRelaySupervisor::start(
                spec,
                handle.route.clone(),
                spawner,
                Arc::clone(&self.gram_clock),
                Arc::clone(&self.running),
                Arc::clone(&self.gram_consent),
            ));
        }
        detached
    }

    /// A cheap snapshot of the outbound proxy registry for the hot per-connection
    /// path: a brief read lock and an `Arc::clone`. The returned `Arc` is a
    /// consistent view even if a reconcile swaps the map immediately after.
    pub fn registry_snapshot(&self) -> Arc<HashMap<String, PeerRoute>> {
        let registry = self
            .registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(&registry)
    }

    pub fn machine_statuses_with_endpoint_statuses(
        &self,
        endpoint_statuses: &HashMap<String, MachineEndpointStatus>,
    ) -> BTreeMap<String, CoordinatorMachineStatus> {
        self.machine_statuses_from_profiles(None, endpoint_statuses)
    }

    #[cfg(test)]
    fn machine_statuses_with_profiles(
        &self,
        profile_override: Option<&[crate::client::endpoint::SavedSshEndpoint]>,
    ) -> BTreeMap<String, CoordinatorMachineStatus> {
        self.machine_statuses_from_profiles(profile_override, &HashMap::new())
    }

    fn machine_statuses_from_profiles(
        &self,
        profile_override: Option<&[crate::client::endpoint::SavedSshEndpoint]>,
        endpoint_statuses: &HashMap<String, MachineEndpointStatus>,
    ) -> BTreeMap<String, CoordinatorMachineStatus> {
        const STALE_AFTER: Duration = Duration::from_secs(15);
        let loaded_profiles = profile_override
            .is_none()
            .then(crate::client::endpoint::EndpointCatalog::load_profiles)
            .and_then(Result::ok);
        let source = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let explicit_aliases: std::collections::HashSet<&str> = source
            .explicit
            .iter()
            .map(|peer| peer.alias.as_str())
            .collect();
        let saved_owned_aliases: std::collections::HashSet<String> = source
            .profiles
            .iter()
            .filter(|profile| {
                profile.enabled
                    && source.policies.contains_key(&profile.id)
                    && !explicit_aliases.contains(profile.id.as_str())
            })
            .map(|profile| profile.id.to_string())
            .collect();
        let mut gram_relay_states = self.gram_relay_states();
        let store = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());

        profile_override
            .or(loaded_profiles.as_deref())
            .unwrap_or(&source.profiles)
            .iter()
            .map(|profile| {
                let profile_id = profile.id.to_string();
                let trusted = source.policies.contains_key(&profile.id);
                let saved_state = if !profile.enabled {
                    SavedMachineState::Disabled
                } else if !trusted {
                    SavedMachineState::Untrusted
                } else if !source.enabled {
                    SavedMachineState::CoordinatorDisabled
                } else {
                    SavedMachineState::Coordinated
                };
                let peer = saved_owned_aliases
                    .contains(&profile_id)
                    .then(|| store.peer(&profile_id))
                    .flatten();
                let stale = saved_state == SavedMachineState::Coordinated
                    && peer.is_none_or(|entry| {
                        entry.reachability != crate::api::federation_store::Reachability::Reachable
                            || entry.last_success_unix_ms.is_none_or(|last_success| {
                                now_unix_ms.is_none_or(|now| {
                                    now.saturating_sub(last_success)
                                        > u64::try_from(STALE_AFTER.as_millis()).unwrap_or(u64::MAX)
                                })
                            })
                    });
                let status = CoordinatorMachineStatus {
                    profile_id: profile_id.clone(),
                    display_label: profile.label.clone(),
                    remote_session: profile.session.clone(),
                    saved_state,
                    federation_configured: trusted,
                    endpoint_status: if profile.enabled {
                        endpoint_statuses.get(&profile_id).copied()
                    } else {
                        Some(MachineEndpointStatus::Disabled)
                    },
                    validated_machine_id: peer
                        .and_then(|entry| entry.observation.validated_machine_id.clone()),
                    remote_boot_id: peer.and_then(|entry| entry.observation.remote_boot_id.clone()),
                    federation_reachability: peer.map(|entry| entry.reachability),
                    last_success_unix_ms: peer.and_then(|entry| entry.last_success_unix_ms),
                    last_error_class: peer.and_then(|entry| entry.last_error_class),
                    remote_version: peer.and_then(|entry| entry.observation.remote_version.clone()),
                    remote_protocol: peer.and_then(|entry| entry.observation.remote_protocol),
                    remote_capabilities: peer
                        .and_then(|entry| entry.observation.remote_capabilities.clone()),
                    stale,
                    gram_relay: gram_relay_states.remove(&profile_id).or_else(|| {
                        self.gram_relay_consented(&profile_id)
                            .then_some(crate::api::schema::GramGatewayState::Off)
                    }),
                };
                (profile_id, status)
            })
            .collect()
    }

    /// Apply explicit federation config and coordinator saved-machine policy
    /// for the active session. A successful catalog read is folded in
    /// immediately; later catalog writes are detected by the manager's watcher
    /// without restarting the server.
    pub fn reconcile_config(&self, config: &FederationConfig) {
        self.reconcile_config_for_session(config, crate::session::active_name().as_deref());
    }

    /// [`Self::reconcile_config`] for an explicit session (`None` = default),
    /// after [`FederationConfig::for_session`] drops the coordinator role and
    /// outbound peers of a named session not in `federation.named_sessions`.
    pub(crate) fn reconcile_config_for_session(
        &self,
        config: &FederationConfig,
        session: Option<&str>,
    ) {
        let config = config.for_session(session);
        let suppressed = matches!(config, std::borrow::Cow::Owned(_));
        if suppressed && !self.session_suppressed.swap(true, Ordering::Relaxed) {
            tracing::info!(
                session = session.unwrap_or_default(),
                "federation coordinator, outbound peers and listener are off in this named \
                 session; only the default session federates unless the session is listed in \
                 federation.named_sessions"
            );
        } else if !suppressed {
            self.session_suppressed.store(false, Ordering::Relaxed);
        }
        let loaded_profiles = if config.coordinator {
            match crate::client::endpoint::EndpointCatalog::load_profiles() {
                Ok(profiles) => Some(profiles),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "saved-machine federation catalog reload failed; keeping last good routes"
                    );
                    None
                }
            }
        } else {
            None
        };
        let mut source = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        source.enabled = config.coordinator;
        source.explicit.clone_from(&config.peers);
        source.policies.clone_from(&config.saved_machines);
        if let Some(profiles) = loaded_profiles {
            source.profiles = profiles;
        }
        let desired = compose_desired(&source);
        self.reconcile_with_grants(&desired, &source.policies);
    }

    fn refresh_saved_profiles(&self) {
        let enabled = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .enabled;
        if !enabled {
            return;
        }
        let profiles = match crate::client::endpoint::EndpointCatalog::load_profiles() {
            Ok(profiles) => profiles,
            Err(error) => {
                tracing::warn!(%error, "saved-machine federation catalog reload failed; keeping last good routes");
                return;
            }
        };
        let mut source = self
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if source.profiles == profiles {
            return;
        }
        source.profiles = profiles;
        let desired = compose_desired(&source);
        self.reconcile_with_grants(&desired, &source.policies);
    }

    #[cfg(test)]
    pub fn reconcile(&self, desired: &[FederationPeer]) {
        self.reconcile_with_grants(desired, &BTreeMap::new());
    }

    /// Reconcile the running outbound peer set against `desired`.
    ///
    /// Holds the `handles` lock for the whole call (so two reconciles serialize)
    /// and follows the lock order **H → S → R**:
    /// 1. Reap any finished retiring threads.
    /// 2. Compute the desired OUTBOUND set (peers with an `endpoint`) by alias.
    /// 3. For each running alias no longer desired, or whose connection/trust
    ///    spec (`endpoint`, `token_file`, `expected_node_id`, `profile_id`, or
    ///    `remote_session`) changed: set its `stop` flag AND evict its store
    ///    entry under the store lock, then detach its retirement into the reaper.
    /// 4. For each desired outbound alias not already running: spawn a fresh
    ///    poll thread.
    /// 5. Rebuild and swap the proxy registry from the same live routes used by
    ///    pollers.
    fn reconcile_with_grants(
        &self,
        desired: &[FederationPeer],
        policies: &BTreeMap<crate::client::endpoint::ProfileId, FederationSavedMachinePolicy>,
    ) {
        let _reconcile = self
            .reconcile_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        let mut handles = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // 1. Join any retiring threads that have already finished.
        self.reap_finished();

        // 2. Desired OUTBOUND peers (an `endpoint` is present), by alias. A peer
        //    without an endpoint is inbound-only and spawns nothing.
        let mut desired_out: HashMap<&str, &FederationPeer> = HashMap::new();
        for peer in desired {
            if peer.endpoint.is_some() {
                desired_out.insert(peer.alias.as_str(), peer);
            }
        }

        // 3. Stop + evict every running alias that is gone or whose spec changed.
        let to_stop: Vec<String> = handles
            .iter()
            .filter(|(alias, handle)| match desired_out.get(alias.as_str()) {
                None => true,
                Some(peer) => {
                    spec_differs(handle, peer)
                        || handle.reverse_grants != grants_for(peer, policies)
                }
            })
            .map(|(alias, _)| alias.clone())
            .collect();
        let mut bridge_retirements = Vec::new();
        #[cfg(unix)]
        let mut retired_gateways = Vec::new();
        #[cfg(unix)]
        let mut retired_gram_relays = Vec::new();
        for alias in &to_stop {
            let Some(handle) = handles.remove(alias) else {
                continue;
            };
            // Stop before retiring the route, so the relay thread, which sees
            // a retired route as stale and reconnects at once, exits instead
            // of spinning until stop lands.
            handle.stop.store(true, Ordering::Relaxed);
            handle.route.retire();
            // S (brief): evict the alias while holding the store Mutex, after
            // stop is set, so a retiring thread's under-lock stop check (see
            // `poll_once_into_cache`) can never write a stale entry afterward.
            self.store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove_peer(alias);
            #[cfg(unix)]
            if let Some(gateway) = handle._reverse_gateway {
                retired_gateways.push(gateway);
            }
            #[cfg(unix)]
            retired_gram_relays.extend(handle.gram_relay);
            let PeerHandle {
                join, _ssh_bridge, ..
            } = handle;
            if let Some(bridge) = _ssh_bridge {
                bridge_retirements.push(bridge.retire());
            }
            let mut reaper = self
                .reaper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            reaper.push(retire_poll(join));
            info!(alias = %alias, "federation peer stopped and evicted (reconcile)");
        }
        // Supervisor joins run concurrently and never hold the handle registry
        // lock. Waiting here closes stable profile sockets before replacement,
        // and stops each retired Gram gateway before a replacement can start.
        drop(handles);
        #[cfg(unix)]
        drop(retired_gateways);
        #[cfg(unix)]
        drop(retired_gram_relays);
        for join in bridge_retirements {
            let _ = join.join();
        }
        let mut handles = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // 4. Update display-only presentation in place, or spawn a poll thread
        //    for every newly desired outbound alias.
        for (alias, peer) in &desired_out {
            if let Some(handle) = handles.get(*alias) {
                let presentation = PeerPresentation::from_peer(peer);
                *handle
                    .presentation
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = presentation.clone();
                self.store
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .update_peer_presentation(
                        alias,
                        presentation.profile_id.as_deref(),
                        &presentation.label,
                    );
                debug!(alias = %alias, "federation peer route unchanged; presentation updated");
                continue;
            }
            if let Some(handle) = self.spawn_peer_poll((*peer).clone(), grants_for(peer, policies))
            {
                handles.insert((*alias).to_string(), handle);
                info!(alias = %alias, "federation peer spawned (reconcile)");
            }
        }

        // 5. Start or stop Gram reverse gateway supervisors against consent.
        #[cfg(unix)]
        let detached_gram_relays = self.sync_gram_relays(&mut handles);

        // 6. Rebuild the outbound proxy registry from the same routes held by
        // the live pollers. SSH clones point at the manager-owned local bridge.
        let new_map = handles
            .iter()
            .map(|(alias, handle)| (alias.clone(), handle.route.clone()))
            .collect();
        let mut registry = self
            .registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *registry = Arc::new(new_map);
        drop(registry);
        drop(handles);
        #[cfg(unix)]
        drop(detached_gram_relays);
    }

    /// Resolve one outbound route and spawn its poll and event relay threads.
    /// SSH peers use the upstream saved-machine `remote-api-bridge`; the
    /// manager owns that bridge and shares its local socket with polling,
    /// relaying and proxying.
    fn spawn_peer_poll(
        &self,
        peer: FederationPeer,
        reverse_grants: Vec<crate::config::FederationAgentGrant>,
    ) -> Option<PeerHandle> {
        let endpoint = peer.endpoint.as_deref()?;
        let token = endpoint
            .starts_with("tcp://")
            .then(|| read_peer_token(&peer))
            .flatten();
        let stop = Arc::new(AtomicBool::new(false));
        let parsed = match crate::api::client::endpoint_to_target(endpoint, token.clone()) {
            Ok(target) => target,
            Err(error) => {
                tracing::warn!(alias = %peer.alias, %endpoint, %error, "invalid federation endpoint; peer not started");
                return None;
            }
        };
        let (route, ssh_bridge, peer_channel) = match parsed {
            crate::api::client::FederationEndpoint::Ssh { destination } => {
                let Some(profile_id) = peer.profile_id.as_deref() else {
                    tracing::warn!(alias = %peer.alias, "SSH federation peer requires profile_id");
                    return None;
                };
                let Some(session) = peer.remote_session.as_deref() else {
                    tracing::warn!(alias = %peer.alias, "SSH federation peer requires remote_session");
                    return None;
                };
                let target = destination;
                let (route, bridge) = match start_saved_peer_bridge(
                    profile_id,
                    &target,
                    session,
                    Arc::clone(&self.running),
                    Arc::clone(&stop),
                    peer.alias.clone(),
                    Arc::clone(&self.store),
                ) {
                    Ok(started) => started,
                    Err(error) => {
                        let mut tracker =
                            crate::api::federation_store::ReachabilityTracker::default();
                        record_saved_bridge_failure(&self.store, &peer.alias, &mut tracker, &error);
                        tracing::warn!(alias = %peer.alias, %error, "SSH federation remote-api-bridge did not start");
                        return None;
                    }
                };
                (
                    route,
                    Some(bridge),
                    Some(saved_peer_channel(profile_id, &target, session)),
                )
            }
            crate::api::client::FederationEndpoint::Target(target) => (target, None, None),
        };
        let presentation = Arc::new(RwLock::new(PeerPresentation::from_peer(&peer)));
        let mut route = PeerRoute::new(
            route,
            Arc::clone(&self.generation_clock),
            peer.expected_node_id.is_some(),
        )
        .with_presentation(Arc::clone(&presentation));
        if let Some(channel) = peer_channel {
            route = route.with_peer_channel(channel);
        }
        // The gateway itself is supervised by `sync_gram_relays`, independently
        // of this bridge, so a failed attempt is retried without a respawn.
        #[cfg(unix)]
        let gram_spec = crate::api::gram_gateway::GramGatewaySpec::for_peer(&peer);
        #[cfg(unix)]
        if gram_spec.is_none() && (self.gram_consent)(&peer.alias) {
            tracing::warn!(alias = %peer.alias, "Gram reverse gateway requires a pinned saved SSH peer");
        }
        let endpoint = peer.endpoint.clone().unwrap_or_default();
        #[cfg(unix)]
        let reverse_gateway = if reverse_grants.is_empty() {
            None
        } else {
            match (
                peer.profile_id.as_deref(),
                peer.remote_session.as_deref(),
                peer.expected_node_id.as_deref(),
            ) {
                (Some(profile), Some(_session), Some(machine)) => {
                    let target = endpoint.strip_prefix("ssh://").unwrap_or_default();
                    match crate::api::reverse_agents::ReverseGateway::start(
                        profile,
                        target,
                        machine,
                        route.clone(),
                        reverse_grants.clone(),
                    ) {
                        Ok(gateway) => Some(gateway),
                        Err(error) => {
                            tracing::warn!(alias = %peer.alias, %error, "restricted reverse gateway did not start");
                            None
                        }
                    }
                }
                _ => None,
            }
        };
        let token_file = peer.token_file.clone();
        let expected_node_id = peer.expected_node_id.clone();
        let profile_id = peer.profile_id.clone();
        let remote_session = peer.remote_session.clone();
        let cache = Arc::clone(&self.store);
        let running = Arc::clone(&self.running);
        let thread_stop = Arc::clone(&stop);
        let poll_route = route.clone();
        let poll_presentation = Arc::clone(&presentation);
        // The relay idles until the peer is identity-validated and advertises
        // `events_v2`, so a peer without the capability keeps the poll alone.
        let relay = PeerRelay {
            alias: peer.alias.clone(),
            expected_node_id: peer.expected_node_id.clone(),
            route: route.clone(),
            presentation: Arc::clone(&presentation),
            store: Arc::clone(&self.store),
            event_hub: self.event_hub.clone(),
            running: Arc::clone(&self.running),
            stop: Arc::clone(&stop),
            timing: RelayTiming::default(),
        };
        // One join covers both threads: the poll returns on the same stop and
        // shutdown flags the relay watches, then waits for the relay.
        let join = std::thread::spawn(move || {
            let relay = std::thread::spawn(move || relay.run());
            run_federation_peer_poll(
                peer,
                poll_route,
                poll_presentation,
                cache,
                running,
                thread_stop,
            );
            let _ = relay.join();
        });
        Some(PeerHandle {
            stop,
            join,
            endpoint,
            token_file,
            expected_node_id,
            token,
            profile_id,
            #[cfg(unix)]
            gram_spec,
            #[cfg(unix)]
            gram_relay: None,
            remote_session,
            route,
            presentation,
            #[cfg(unix)]
            _reverse_gateway: reverse_gateway,
            reverse_grants,
            _ssh_bridge: ssh_bridge,
        })
    }

    /// Join every retiring thread that has already finished, leaving the rest in
    /// the reaper. Never blocks: a thread that is still running is left for a
    /// later reap. Takes only the `reaper` lock.
    fn reap_finished(&self) {
        let mut reaper = self
            .reaper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pending = std::mem::take(&mut *reaper);
        for join in pending {
            if join.is_finished() {
                let _ = join.join();
            } else {
                reaper.push(join);
            }
        }
    }

    /// Stop the catalog watcher and every poll thread, then join every retiring
    /// thread. Called once from [`ServerHandle`](crate::api::ServerHandle)'s
    /// drop, where a blocking join is acceptable. Drains `handles` before
    /// joining so no lock is held across a poll join.
    pub fn join_all(&self) {
        self.catalog_stop.store(true, Ordering::Release);
        {
            let barrier = self
                .reconcile_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(barrier);
        }
        let watcher = self
            .catalog_watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(watcher) = watcher {
            let _ = watcher.join();
        }
        let live: Vec<PeerHandle> = {
            let mut handles = self
                .handles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            handles.drain().map(|(_, handle)| handle).collect()
        };
        for handle in &live {
            handle.stop.store(true, Ordering::Relaxed);
            handle.route.retire();
            handle.stop.store(true, Ordering::Relaxed);
        }
        let mut poll_joins = Vec::with_capacity(live.len());
        let mut bridge_joins = Vec::new();
        for handle in live {
            let PeerHandle {
                join, _ssh_bridge, ..
            } = handle;
            poll_joins.push(join);
            if let Some(bridge) = _ssh_bridge {
                bridge_joins.push(bridge.retire());
            }
        }
        for join in poll_joins {
            let _ = join.join();
        }
        for join in bridge_joins {
            let _ = join.join();
        }
        let pending: Vec<JoinHandle<()>> = {
            let mut reaper = self
                .reaper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *reaper)
        };
        for join in pending {
            let _ = join.join();
        }
    }

    /// Aliases with a currently running poll thread, for tests.
    #[cfg(test)]
    pub(crate) fn live_aliases(&self) -> Vec<String> {
        let handles = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut aliases: Vec<String> = handles.keys().cloned().collect();
        aliases.sort();
        aliases
    }

    /// Current mutable presentation for one live route, for reload tests.
    #[cfg(all(test, unix))]
    pub(crate) fn presentation_for(&self, alias: &str) -> Option<PeerPresentation> {
        let handles = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        handles.get(alias).map(|handle| {
            handle
                .presentation
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
    }

    /// Remote session attached to one live transport generation, for reload tests.
    #[cfg(all(test, unix))]
    pub(crate) fn remote_session_for(&self, alias: &str) -> Option<String> {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(alias)
            .and_then(|handle| handle.remote_session.clone())
    }

    /// Gram gateway state and transport identity of one live peer, for tests.
    #[cfg(all(test, unix))]
    pub(crate) fn gram_relay_for(
        &self,
        alias: &str,
    ) -> Option<(Option<crate::api::schema::GramGatewayState>, usize)> {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(alias)
            .map(|handle| {
                (
                    handle
                        .gram_relay
                        .as_ref()
                        .map(crate::api::gram_gateway::GramRelaySupervisor::state),
                    Arc::as_ptr(&handle.stop) as usize,
                )
            })
    }

    /// Reap finished retiring threads, then report how many are still pending,
    /// for tests polling that a retired thread has actually finished.
    #[cfg(test)]
    pub(crate) fn reap_and_count_pending(&self) -> usize {
        self.reap_finished();
        let reaper = self
            .reaper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reaper.len()
    }

    #[cfg(test)]
    fn stop_catalog_watcher_for_test(&self) {
        self.catalog_stop.store(true, Ordering::Release);
        if let Some(watcher) = self
            .catalog_watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = watcher.join();
        }
    }
}

fn watch_saved_profiles(manager: Weak<FederationPeerManager>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        let Some(manager) = manager.upgrade() else {
            return;
        };
        manager.refresh_saved_profiles();
        drop(manager);
        for _ in 0..10 {
            if stop.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn compose_desired(source: &CoordinatorSource) -> Vec<FederationPeer> {
    let mut desired = source.explicit.clone();
    if !source.enabled {
        return desired;
    }
    let mut aliases: std::collections::HashSet<String> =
        desired.iter().map(|peer| peer.alias.clone()).collect();
    for profile in source.profiles.iter().filter(|profile| profile.enabled) {
        let Some(policy) = source.policies.get(&profile.id) else {
            continue;
        };
        let alias = profile.id.to_string();
        if !aliases.insert(alias.clone()) {
            tracing::warn!(
                %alias,
                "saved-machine federation alias collides with an explicit peer; explicit peer wins"
            );
            continue;
        }
        desired.push(FederationPeer {
            alias,
            display_label: Some(profile.label.clone()),
            endpoint: Some(format!(
                "ssh://{}",
                profile
                    .target
                    .strip_prefix("ssh://")
                    .unwrap_or(&profile.target)
            )),
            profile_id: Some(profile.id.to_string()),
            remote_session: Some(profile.session.clone()),
            token_file: None,
            expected_node_id: Some(policy.expected_machine_id.clone()),
            capability: crate::config::CapabilityTier::Admin,
        });
    }
    desired
}

/// Whether a running peer's connection or trust spec differs from the desired
/// config, so its thread must be retired and respawned.
fn grants_for(
    peer: &FederationPeer,
    policies: &BTreeMap<crate::client::endpoint::ProfileId, FederationSavedMachinePolicy>,
) -> Vec<crate::config::FederationAgentGrant> {
    peer.profile_id
        .as_deref()
        .and_then(|id| crate::client::endpoint::ProfileId::parse(id.to_owned()).ok())
        .and_then(|id| policies.get(&id))
        .map(|policy| policy.agent_grants.clone())
        .unwrap_or_default()
}

fn spec_differs(handle: &PeerHandle, peer: &FederationPeer) -> bool {
    let token = peer
        .endpoint
        .as_deref()
        .is_some_and(|endpoint| endpoint.starts_with("tcp://"))
        .then(|| read_peer_token(peer))
        .flatten();
    handle.endpoint != peer.endpoint.clone().unwrap_or_default()
        || handle.token_file != peer.token_file
        || handle.token != token
        || handle.expected_node_id != peer.expected_node_id
        || handle.profile_id != peer.profile_id
        || handle.remote_session != peer.remote_session
}

/// The persistent `api-bridge --multi` channel for a saved peer, launched
/// through the same managed SSH transport as its bridge.
fn saved_peer_channel(profile_id: &str, target: &str, session: &str) -> Arc<PeerChannel> {
    let profile_id = profile_id.to_owned();
    let target = target.to_owned();
    let session = session.to_owned();
    PeerChannel::new(Arc::new(move || {
        crate::remote::saved_peer_api_channel_command(&profile_id, &target, &session)
    }))
}

fn start_saved_peer_bridge(
    profile_id: &str,
    target: &str,
    session: &str,
    running: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    alias: String,
    store: Arc<Mutex<FederationStore>>,
) -> io::Result<(ConnectionTarget, SavedBridgeSupervisor)> {
    let cancellation = Arc::new(AtomicBool::new(false));
    let bridge = crate::remote::SavedSshApiBridge::start_cancellable(
        profile_id,
        target,
        session,
        true,
        Arc::clone(&cancellation),
    )?;
    let path = bridge.socket_path().to_owned();
    let profile_id = profile_id.to_owned();
    let target = target.to_owned();
    let session = session.to_owned();
    let thread_cancellation = Arc::clone(&cancellation);
    let join = std::thread::spawn(move || {
        supervise_saved_peer_bridge(
            bridge,
            &profile_id,
            &target,
            &session,
            &running,
            &stop,
            &thread_cancellation,
            &alias,
            &store,
        );
    });
    Ok((
        ConnectionTarget::SocketPath(path),
        SavedBridgeSupervisor {
            join: Some(join),
            cancellation,
        },
    ))
}

fn saved_bridge_failure_class(error: &io::Error) -> crate::api::schema::FederationPollErrorClass {
    let message = error.to_string().to_ascii_lowercase();
    if error.kind() == io::ErrorKind::PermissionDenied
        || message.contains("permission denied")
        || message.contains("authentication")
    {
        crate::api::schema::FederationPollErrorClass::AuthenticationFailed
    } else {
        crate::api::schema::FederationPollErrorClass::Transport
    }
}

fn record_saved_bridge_failure(
    store: &Mutex<FederationStore>,
    alias: &str,
    tracker: &mut crate::api::federation_store::ReachabilityTracker,
    error: &io::Error,
) {
    let reachability = tracker.record_miss();
    let class = saved_bridge_failure_class(error);
    let mut store = store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.degrade_peer(alias, reachability);
    store.record_poll_error(alias, class);
}

fn supervise_saved_peer_bridge(
    initial: crate::remote::SavedSshApiBridge,
    profile_id: &str,
    target: &str,
    session: &str,
    running: &AtomicBool,
    stop: &AtomicBool,
    cancellation: &Arc<AtomicBool>,
    alias: &str,
    store: &Mutex<FederationStore>,
) {
    let mut failure_tracker = crate::api::federation_store::ReachabilityTracker::default();
    let mut bridge = Some(initial);
    let mut use_cached_metadata = true;
    while !bridge_stopping(running, stop, cancellation) {
        if let Some(active) = bridge.as_ref() {
            let failure = match active.try_reported_failure() {
                Ok(Some(failure)) => failure,
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(failure) => failure,
            };
            if bridge_stopping(running, stop, cancellation) {
                break;
            }
            use_cached_metadata =
                !crate::remote::SavedSshApiBridge::stale_metadata_failure(&failure);
            if !use_cached_metadata {
                active.invalidate_metadata();
            }
            tracing::warn!(%failure, "saved federation bridge exited; restarting");
            record_saved_bridge_failure(store, alias, &mut failure_tracker, &failure);
            drop(bridge.take());
        }
        if bridge_stopping(running, stop, cancellation) {
            break;
        }

        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let profile_id = profile_id.to_owned();
        let target = target.to_owned();
        let session = session.to_owned();
        let restart_cancellation = Arc::clone(cancellation);
        let mut restart = Some(std::thread::spawn(move || {
            let result = crate::remote::SavedSshApiBridge::start_cancellable(
                &profile_id,
                &target,
                &session,
                use_cached_metadata,
                restart_cancellation,
            );
            let _ = result_tx.send(result);
        }));

        loop {
            if bridge_stopping(running, stop, cancellation) {
                cancellation.store(true, Ordering::Release);
                if let Some(join) = restart.take() {
                    let _ = join.join();
                }
                return;
            }
            match result_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(started)) => {
                    if let Some(join) = restart.take() {
                        let _ = join.join();
                    }
                    bridge = Some(started);
                    use_cached_metadata = true;
                    break;
                }
                Ok(Err(error)) => {
                    if let Some(join) = restart.take() {
                        let _ = join.join();
                    }
                    tracing::warn!(%error, "saved federation bridge restart failed");
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(join) = restart.take() {
                        let _ = join.join();
                    }
                    tracing::warn!("saved federation bridge restart worker disconnected");
                    break;
                }
            }
        }
        if bridge.is_none() {
            for _ in 0..10 {
                if bridge_stopping(running, stop, cancellation) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    cancellation.store(true, Ordering::Release);
}

fn bridge_stopping(running: &AtomicBool, stop: &AtomicBool, cancellation: &AtomicBool) -> bool {
    !running.load(Ordering::Relaxed)
        || stop.load(Ordering::Relaxed)
        || cancellation.load(Ordering::Acquire)
}

/// Join one stopped poll thread away from the reconcile caller.
fn retire_poll(join: JoinHandle<()>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = join.join();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_applies_in_place_tcp_token_rotation() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let token_path = std::env::temp_dir().join(format!("herdr-federation-token-{nonce}.txt"));
        std::fs::write(&token_path, "old-token\n").unwrap();
        let peer = FederationPeer {
            alias: "build".into(),
            endpoint: Some("tcp://127.0.0.1:9".into()),
            token_file: Some(token_path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::new(Mutex::new(FederationStore::default())),
            EventHub::default(),
            Arc::clone(&running),
        );

        manager.reconcile(std::slice::from_ref(&peer));
        assert_eq!(
            route_token(&manager.registry_snapshot()["build"]),
            Some("old-token")
        );

        std::fs::write(&token_path, "new-token\n").unwrap();
        manager.reconcile(std::slice::from_ref(&peer));
        assert_eq!(
            route_token(&manager.registry_snapshot()["build"]),
            Some("new-token")
        );

        running.store(false, Ordering::Relaxed);
        manager.join_all();
        let _ = std::fs::remove_file(token_path);
    }

    fn route_token(route: &PeerRoute) -> Option<&str> {
        match route.target() {
            ConnectionTarget::Tcp { token, .. } => token.as_deref(),
            _ => panic!("expected TCP route"),
        }
    }

    #[test]
    fn reconcile_config_applies_named_session_rule_on_every_reload() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let token_path =
            std::env::temp_dir().join(format!("herdr-federation-session-token-{nonce}.txt"));
        std::fs::write(&token_path, "token\n").unwrap();
        let profile = crate::client::endpoint::SavedSshEndpoint::new(
            "Build",
            "dev@build.example",
            "agent-work",
        )
        .unwrap();
        let mut config = FederationConfig {
            coordinator: true,
            peers: vec![FederationPeer {
                alias: "build".into(),
                endpoint: Some("tcp://127.0.0.1:9".into()),
                token_file: Some(token_path.to_string_lossy().into_owned()),
                ..Default::default()
            }],
            ..Default::default()
        };
        config.saved_machines.insert(
            profile.id.clone(),
            FederationSavedMachinePolicy {
                expected_machine_id: "machine_build".into(),
                agent_grants: Vec::new(),
            },
        );
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::new(Mutex::new(FederationStore::default())),
            EventHub::default(),
            Arc::clone(&running),
        );
        manager.stop_catalog_watcher_for_test();
        let coordinates = |manager: &FederationPeerManager| {
            let source = manager
                .coordinator
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Probe with the trusted saved profile present, without letting the
            // manager spawn an SSH bridge for it.
            let mut probe = source.clone();
            probe.profiles = vec![profile.clone()];
            let saved_machine_desired = compose_desired(&probe)
                .iter()
                .any(|peer| peer.alias == profile.id.as_str());
            assert_eq!(source.enabled, saved_machine_desired);
            source.enabled
        };
        let routed =
            |manager: &FederationPeerManager| manager.registry_snapshot().contains_key("build");

        // Boot and every reload of an unlisted named session: no coordinator
        // role, no saved machines, no outbound explicit peer.
        manager.reconcile_config_for_session(&config, Some("work"));
        assert!(!coordinates(&manager));
        assert!(!routed(&manager));
        manager.reconcile_config_for_session(&config, Some("work"));
        assert!(!routed(&manager));

        // Reload that lists the session federates exactly like the default one.
        config.named_sessions = vec!["work".into()];
        manager.reconcile_config_for_session(&config, Some("work"));
        assert!(coordinates(&manager));
        assert!(routed(&manager));

        // Reload that unlists it again tears the outbound peer back down.
        config.named_sessions.clear();
        manager.reconcile_config_for_session(&config, Some("work"));
        assert!(!coordinates(&manager));
        assert!(!routed(&manager));

        // The default session is never suppressed.
        manager.reconcile_config_for_session(&config, None);
        assert!(coordinates(&manager));
        assert!(routed(&manager));

        running.store(false, Ordering::Relaxed);
        manager.join_all();
        let _ = std::fs::remove_file(token_path);
    }

    #[test]
    fn coordinator_adapts_enabled_trusted_profiles_without_label_routing() {
        let mut profile = crate::client::endpoint::SavedSshEndpoint::new(
            "Build",
            "ssh://dev@build.example",
            "agent-work",
        )
        .unwrap();
        let profile_id = profile.id.clone();
        let mut source = CoordinatorSource {
            enabled: true,
            explicit: Vec::new(),
            policies: BTreeMap::from([(
                profile_id.clone(),
                FederationSavedMachinePolicy {
                    expected_machine_id: "machine_build".into(),
                    agent_grants: Vec::new(),
                },
            )]),
            profiles: vec![profile.clone()],
        };

        let desired = compose_desired(&source);
        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].alias, profile_id.as_str());
        assert_eq!(desired[0].display_label.as_deref(), Some("Build"));
        assert_eq!(
            desired[0].endpoint.as_deref(),
            Some("ssh://dev@build.example")
        );
        assert_eq!(desired[0].remote_session.as_deref(), Some("agent-work"));
        assert_eq!(
            desired[0].expected_node_id.as_deref(),
            Some("machine_build")
        );

        profile.label = "Renamed".into();
        source.profiles = vec![profile.clone()];
        let renamed = compose_desired(&source);
        assert_eq!(renamed[0].alias, desired[0].alias);
        assert_eq!(renamed[0].endpoint, desired[0].endpoint);
        assert_eq!(renamed[0].display_label.as_deref(), Some("Renamed"));

        profile.enabled = false;
        source.profiles = vec![profile];
        assert!(compose_desired(&source).is_empty());
    }

    #[test]
    fn coordinator_role_and_profile_identity_fail_closed() {
        let profile = crate::client::endpoint::SavedSshEndpoint::new(
            "Build",
            "dev@build.example",
            "agent-work",
        )
        .unwrap();
        let mut source = CoordinatorSource {
            enabled: false,
            explicit: Vec::new(),
            policies: BTreeMap::from([(
                profile.id.clone(),
                FederationSavedMachinePolicy {
                    expected_machine_id: "machine_build".into(),
                    agent_grants: Vec::new(),
                },
            )]),
            profiles: vec![profile.clone()],
        };
        assert!(
            compose_desired(&source).is_empty(),
            "remote/default role must not auto-federate saved machines"
        );

        source.enabled = true;
        let replacement = crate::client::endpoint::SavedSshEndpoint::new(
            "Build",
            "dev@build.example",
            "agent-work",
        )
        .unwrap();
        assert_ne!(replacement.id, profile.id);
        source.profiles = vec![replacement];
        assert!(
            compose_desired(&source).is_empty(),
            "remove/re-add must not inherit trust keyed to the old profile id"
        );
    }

    #[test]
    fn machine_status_separates_saved_endpoint_and_federation_health() {
        let mut profile = crate::client::endpoint::SavedSshEndpoint::new(
            "Build",
            "dev@build.example",
            "agent-work",
        )
        .unwrap();
        let profile_id = profile.id.clone();
        let profile_text = profile_id.to_string();
        let store = Arc::new(Mutex::new(FederationStore::default()));
        let manager = FederationPeerManager::new(
            Arc::clone(&store),
            EventHub::default(),
            Arc::new(AtomicBool::new(true)),
        );
        manager.stop_catalog_watcher_for_test();
        *manager
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = CoordinatorSource {
            enabled: true,
            explicit: Vec::new(),
            policies: BTreeMap::from([(
                profile_id,
                FederationSavedMachinePolicy {
                    expected_machine_id: "machine_build".into(),
                    agent_grants: Vec::new(),
                },
            )]),
            profiles: vec![profile.clone()],
        };

        let endpoint_statuses =
            HashMap::from([(profile_text.clone(), MachineEndpointStatus::Online)]);
        let pending = manager.machine_statuses_from_profiles(
            Some(std::slice::from_ref(&profile)),
            &endpoint_statuses,
        );
        assert_eq!(
            pending[&profile_text].saved_state,
            SavedMachineState::Coordinated
        );
        assert_eq!(
            pending[&profile_text].endpoint_status,
            Some(MachineEndpointStatus::Online)
        );
        assert_eq!(pending[&profile_text].federation_reachability, None);
        assert!(pending[&profile_text].stale);

        store.lock().unwrap().set_peer(
            profile_text.clone(),
            crate::api::federation_store::PeerCacheEntry::reachable_observed(
                Vec::new(),
                std::time::Instant::now(),
                crate::api::federation_store::PeerObservation {
                    validated_machine_id: Some("machine_build".into()),
                    remote_boot_id: Some("boot-1".into()),
                    remote_version: Some("0.9.1".into()),
                    remote_protocol: Some(2),
                    remote_capabilities: crate::api::default_capabilities(),
                },
            ),
        );
        let online = manager.machine_statuses_from_profiles(
            Some(std::slice::from_ref(&profile)),
            &endpoint_statuses,
        );
        assert_eq!(
            online[&profile_text].federation_reachability,
            Some(crate::api::federation_store::Reachability::Reachable)
        );
        assert_eq!(
            online[&profile_text].validated_machine_id.as_deref(),
            Some("machine_build")
        );
        assert_eq!(
            online[&profile_text].remote_boot_id.as_deref(),
            Some("boot-1")
        );
        assert_eq!(online[&profile_text].remote_protocol, Some(2));
        assert!(!online[&profile_text].stale);
        assert!(online[&profile_text].federation_configured);
        let encoded = serde_json::to_string(&online[&profile_text]).unwrap();
        assert!(!encoded.contains("dev@build.example"));
        assert!(!encoded.contains("token"));

        profile.enabled = false;
        manager
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .profiles = vec![profile.clone()];
        let disabled = manager.machine_statuses_with_profiles(Some(std::slice::from_ref(&profile)));
        assert_eq!(
            disabled[&profile_text].endpoint_status,
            Some(MachineEndpointStatus::Disabled)
        );
        assert_eq!(
            disabled[&profile_text].saved_state,
            SavedMachineState::Disabled
        );
        assert!(!disabled[&profile_text].stale);
        assert!(disabled[&profile_text].federation_configured);
        manager
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .policies
            .clear();
        let disabled_without_policy =
            manager.machine_statuses_with_profiles(Some(std::slice::from_ref(&profile)));
        assert!(!disabled_without_policy[&profile_text].federation_configured);

        manager.join_all();
    }

    #[test]
    fn machine_status_ignores_explicit_peer_cache_on_profile_alias_collision() {
        let profile = crate::client::endpoint::SavedSshEndpoint::new(
            "Build",
            "dev@build.example",
            "agent-work",
        )
        .unwrap();
        let alias = profile.id.to_string();
        let store = Arc::new(Mutex::new(FederationStore::default()));
        store.lock().unwrap().degrade_peer(
            &alias,
            crate::api::federation_store::Reachability::Unreachable,
        );
        let manager = FederationPeerManager::new(
            Arc::clone(&store),
            EventHub::default(),
            Arc::new(AtomicBool::new(true)),
        );
        manager.stop_catalog_watcher_for_test();
        *manager
            .coordinator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = CoordinatorSource {
            enabled: true,
            explicit: vec![FederationPeer {
                alias: alias.clone(),
                display_label: None,
                endpoint: Some("ssh://dev@explicit.example".into()),
                profile_id: Some(profile.id.to_string()),
                remote_session: Some("explicit-session".into()),
                token_file: None,
                expected_node_id: None,
                capability: crate::config::CapabilityTier::Admin,
            }],
            policies: BTreeMap::from([(
                profile.id.clone(),
                FederationSavedMachinePolicy {
                    expected_machine_id: "machine_build".into(),
                    agent_grants: Vec::new(),
                },
            )]),
            profiles: vec![profile.clone()],
        };

        let statuses = manager.machine_statuses_with_profiles(Some(std::slice::from_ref(&profile)));
        assert_eq!(statuses[&alias].federation_reachability, None);
        assert_eq!(statuses[&alias].last_error_class, None);
        manager.join_all();
    }

    #[test]
    fn saved_bridge_permission_failure_is_classified_as_authentication() {
        let error = io::Error::other("Permission denied (publickey)");
        assert_eq!(
            saved_bridge_failure_class(&error),
            crate::api::schema::FederationPollErrorClass::AuthenticationFailed
        );
    }

    #[cfg(unix)]
    mod gram_relay {
        use super::*;
        use crate::api::gram_gateway::test_support::{wait_until, FakeSpawner, ManualClock};
        use crate::api::schema::GramGatewayState;

        const ALIAS: &str = "8195b6326f748f4da1945364a4e205b9";

        fn pinned_peer() -> FederationPeer {
            FederationPeer {
                alias: ALIAS.into(),
                display_label: Some("mac".into()),
                // Unroutable TCP keeps the test free of SSH; the gateway spawner is fake.
                endpoint: Some("tcp://127.0.0.1:9".into()),
                profile_id: Some(ALIAS.into()),
                remote_session: Some("main".into()),
                token_file: None,
                expected_node_id: Some("machine_cf86dc42c063eac7b83162361990ae59".into()),
                capability: crate::config::CapabilityTier::Admin,
            }
        }

        fn manager(
            spawner: &Arc<FakeSpawner>,
            clock: &Arc<ManualClock>,
        ) -> Arc<FederationPeerManager> {
            FederationPeerManager::with_gram_hooks(
                Arc::new(Mutex::new(FederationStore::default())),
                Arc::new(AtomicBool::new(true)),
                Arc::clone(spawner) as Arc<dyn crate::api::gram_gateway::GramGatewaySpawner>,
                Arc::clone(clock) as Arc<dyn crate::api::gram_gateway::GatewayClock>,
                Arc::new(|alias: &str| alias == ALIAS),
            )
        }

        fn state(manager: &FederationPeerManager) -> Option<GramGatewayState> {
            manager.gram_relay_for(ALIAS).and_then(|(state, _)| state)
        }

        fn transport(manager: &FederationPeerManager) -> usize {
            manager.gram_relay_for(ALIAS).expect("live peer").1
        }

        #[test]
        fn failed_preflight_is_retried_without_restarting_the_peer_bridge() {
            let spawner = FakeSpawner::scripted([Err(
                "remote Gram reverse socket preflight failed: exit status: 255".into(),
            )]);
            let clock = ManualClock::new();
            let manager = manager(&spawner, &clock);
            manager.reconcile(&[pinned_peer()]);
            let bridge = transport(&manager);

            wait_until("first retry scheduled", || {
                matches!(
                    state(&manager),
                    Some(GramGatewayState::Retrying { attempt: 1, .. })
                )
            });
            assert_eq!(
                state(&manager),
                Some(GramGatewayState::Retrying {
                    attempt: 1,
                    next_in_secs: 2,
                    last_error: "remote Gram reverse socket preflight failed: exit status: 255"
                        .into(),
                })
            );
            clock.advance(Duration::from_secs(2));
            wait_until("gateway up", || {
                state(&manager) == Some(GramGatewayState::Up)
            });
            assert_eq!(spawner.calls(), 2);
            assert_eq!(transport(&manager), bridge, "peer bridge was not respawned");
            manager.join_all();
            assert_eq!(spawner.live(), 0);
        }

        #[test]
        fn retiring_a_peer_during_backoff_stops_further_attempts() {
            let spawner = FakeSpawner::failing();
            let clock = ManualClock::new();
            let manager = manager(&spawner, &clock);
            manager.reconcile(&[pinned_peer()]);
            wait_until("retry scheduled", || {
                matches!(state(&manager), Some(GramGatewayState::Retrying { .. }))
            });

            manager.reconcile(&[]);
            assert!(manager.live_aliases().is_empty());
            let attempts = spawner.calls();
            clock.advance(Duration::from_secs(600));
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(spawner.calls(), attempts, "a retired peer never retries");
            manager.join_all();
        }

        #[test]
        fn one_gateway_survives_exit_recovery_and_repeated_reconciles() {
            let spawner = FakeSpawner::scripted([]);
            let clock = ManualClock::new();
            let manager = manager(&spawner, &clock);
            manager.reconcile(&[pinned_peer()]);
            wait_until("gateway up", || {
                state(&manager) == Some(GramGatewayState::Up)
            });

            spawner.exit_latest("Gram relay SSH forward exited: exit status: 255");
            wait_until("retry scheduled", || {
                matches!(state(&manager), Some(GramGatewayState::Retrying { .. }))
            });
            manager.reconcile(&[pinned_peer()]);
            clock.advance(Duration::from_secs(2));
            wait_until("gateway recovered", || {
                state(&manager) == Some(GramGatewayState::Up)
            });
            manager.reconcile(&[pinned_peer()]);
            std::thread::sleep(Duration::from_millis(50));

            assert_eq!(spawner.calls(), 2);
            assert_eq!(spawner.live(), 1);
            assert_eq!(
                spawner.max_live.load(Ordering::Acquire),
                1,
                "never two ssh -R forwards for one peer"
            );
            manager.join_all();
            assert_eq!(spawner.live(), 0);
        }

        #[test]
        fn relay_config_changes_start_and_tear_down_gateways_on_reconcile() {
            for legacy_env in [None, Some(ALIAS)] {
                let policy = Arc::new(crate::api::gram_relay::GramRelayPolicy::default());
                let consent_policy = Arc::clone(&policy);
                let spawner = FakeSpawner::scripted([]);
                let clock = ManualClock::new();
                let manager = FederationPeerManager::with_gram_hooks(
                    Arc::new(Mutex::new(FederationStore::default())),
                    Arc::new(AtomicBool::new(true)),
                    Arc::clone(&spawner) as Arc<dyn crate::api::gram_gateway::GramGatewaySpawner>,
                    Arc::clone(&clock) as Arc<dyn crate::api::gram_gateway::GatewayClock>,
                    Arc::new(move |alias: &str| consent_policy.allows(alias)),
                );
                // Models a daemon still carrying the old HERDR_GRAM_RELAY_PEERS wrapper.
                let environment = crate::api::gram_relay::RelayEnvironment {
                    peers: legacy_env.map(str::to_owned),
                    socket: None,
                };
                let apply = |peers: Option<&[&str]>| {
                    policy.apply(
                        &crate::config::GramRelayConfig {
                            peers: peers
                                .map(|peers| peers.iter().map(|peer| (*peer).to_owned()).collect()),
                            coordinator_machine_id: None,
                        },
                        &environment,
                        || unreachable!("no coordinator pin configured"),
                    );
                };

                if legacy_env.is_none() {
                    apply(None);
                    manager.reconcile(&[pinned_peer()]);
                    assert_eq!(state(&manager), None, "default config grants no gateway");
                    assert_eq!(spawner.calls(), 0);
                }

                apply(Some(&[ALIAS]));
                manager.reconcile(&[pinned_peer()]);
                wait_until("gateway up", || {
                    state(&manager) == Some(GramGatewayState::Up)
                });
                let bridge = transport(&manager);

                // `peers = []` revokes even while the legacy variable names the peer.
                apply(Some(&[]));
                manager.reconcile(&[pinned_peer()]);
                assert!(!policy.allows(ALIAS), "env={legacy_env:?}");
                assert_eq!(state(&manager), None, "env={legacy_env:?}");
                assert_eq!(spawner.live(), 0, "revoked gateway is torn down");
                assert_eq!(transport(&manager), bridge, "peer bridge untouched");
                manager.join_all();
            }
        }

        #[test]
        fn machine_status_reports_the_gateway_state() {
            let spawner = FakeSpawner::failing();
            let clock = ManualClock::new();
            let manager = manager(&spawner, &clock);
            manager.reconcile(&[pinned_peer()]);
            wait_until("retry scheduled", || {
                matches!(state(&manager), Some(GramGatewayState::Retrying { .. }))
            });
            let profile = crate::client::endpoint::SavedSshEndpoint {
                id: crate::client::endpoint::ProfileId::parse(ALIAS).unwrap(),
                label: "mac".into(),
                target: "ssh://mac".into(),
                session: "main".into(),
                enabled: true,
            };
            let statuses = manager.machine_statuses_with_profiles(Some(&[profile]));
            assert!(matches!(
                statuses[ALIAS].gram_relay,
                Some(GramGatewayState::Retrying { attempt: 1, .. })
            ));
            manager.join_all();
        }
    }
}
