use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use interprocess::local_socket::traits::ListenerExt as _;
use tracing::{debug, error, info, warn};

#[cfg(all(test, unix))]
use std::fs;

use crate::api::answer::answer_agent;
use crate::api::client::{
    parse_response_value, ApiClient, ApiClientError, ProxyError, FEDERATION_STREAM_IDLE_TIMEOUT,
};
use crate::api::event_hub::EventHistoryError;
use crate::api::federation::{
    authorized_peer, federation_access, federation_method_policy, FederationAccess,
    FederationHello, PeerContext, FEDERATION_PROTOCOL_VERSION,
};
use crate::api::federation_manager::{
    FederationPeerManager, PeerPresentation, PeerRoute, PeerRouteStamp,
};
use crate::api::federation_store::{
    FederationStore, PeerCacheEntry, PeerObservation, Reachability, ReachabilityTracker,
};
use crate::api::schema::{
    ErrorBody, ErrorResponse, FederationPollErrorClass, Method, PaneStreamParams, Request,
    ResponseResult, ServerCapabilities, SuccessResponse,
};
use crate::api::subscriptions::{history_error, ActiveSubscription};
use crate::api::wait::{prompt_agent, wait_for_agent, wait_for_event, wait_for_output};
use crate::api::{
    request_changes_ui, socket_path, ApiRequestMessage, ApiRequestSender, ApiStream, ApiStreamRead,
    EventHub,
};
use crate::config::{FederationConfig, FederationPeer};
#[cfg(test)]
use crate::ipc::LocalStream;
use crate::ipc::{
    bind_local_listener, is_connection_closed_error, remove_socket_file_if_owned,
    socket_file_identity, SocketFileIdentity,
};
use crate::server::shutdown::{ServerStop, ShutdownReason};

mod gram_upload_stream;
#[cfg(unix)]
mod guest_gate;
mod guest_owner;
#[cfg(unix)]
pub(crate) use guest_gate::serve_guest_stream;
mod pane_input_stream;
mod pane_output_stream;
#[cfg(all(test, unix))]
mod peer_channel_tests;
mod stream_read;
#[cfg(test)]
mod subscription_socket_tests;

/// The single-writer claim on a gram `upload_id`, held by every writer:
/// `gram.upload.stream` for a channel's lifetime, `gram.upload_chunk` for one
/// append, and `finalize` for its size/hash/rename sequence.
pub(crate) use gram_upload_stream::UploadClaim;

const SOCKET_PERMISSION_MODE: u32 = 0o600;
pub(super) const CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(super) const APP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_INITIAL_REQUEST_BYTES: usize = 1024 * 1024;
/// Cap on federation TCP connections handled concurrently. A bounded federation
/// listener cannot be driven to unbounded thread/fd growth by an authenticated
/// (or connect-flooding) peer; connections beyond the cap are refused and closed.
const MAX_FEDERATION_CONNECTIONS: usize = 32;
/// How often each configured peer is polled for its agent list.
pub(super) const FEDERATION_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Cadence of the extra `workspace.list` fetch that rides on a reachable
/// peer's agent poll (see [`WorkspaceRefresh`]): workspaces change rarely, so
/// this is a multiple of [`FEDERATION_POLL_INTERVAL`], not every agent poll.
const FEDERATION_WORKSPACE_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Granularity at which the poll sleep re-checks `running`, so a shutdown is not
/// held up for a whole poll interval.
const FEDERATION_POLL_STEP: Duration = Duration::from_millis(100);
/// Total wall-clock bound on an outbound `agent.list` poll's response read —
/// bounds a stuck peer. Unlike a per-read socket timeout, this caps the WHOLE
/// read, so a peer that trickles bytes just under the socket timeout without ever
/// sending a newline cannot keep the read (and shutdown, which joins the poll
/// threads) running forever.
const FEDERATION_POLL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Byte cap on a single outbound federation poll response. Mirrors the inbound
/// [`MAX_INITIAL_REQUEST_BYTES`] cap: an `agent.list` reply is small, so 1 MiB is
/// generous, and a (malicious or faulty) peer returning a larger line is degraded
/// rather than allowed to drive unbounded allocation — an OOM — on the polling
/// home daemon.
const FEDERATION_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Upper bound on the randomized backoff added after a failed poll.
const FEDERATION_POLL_MAX_JITTER: Duration = Duration::from_secs(2);
/// Total wall-clock bound on a single proxied federation request's response read.
/// Generous enough for a waited `agent.prompt` that blocks on the peer until the
/// remote agent reaches a status (matching the local agent-start ceiling), yet
/// finite so a silently-stalled peer cannot leak the connection thread forever.
/// The proxy runs on the per-connection thread — never the single-threaded app
/// loop — so blocking here for the duration is safe; the byte cap, this deadline,
/// and the `running` flag together keep a malicious or faulty peer from OOMing or
/// hanging the home.
const FEDERATION_PROXY_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Per-frame byte cap for a proxied federated `pane.stream`. The pane output ring
/// frames at ~64 KiB, so the 1 MiB [`FEDERATION_MAX_RESPONSE_BYTES`] scale is
/// generous; a peer sending a single frame larger than this degrades that stream
/// (it is closed) rather than driving unbounded allocation — an OOM — on the home.
/// The idle-timeout companion is [`FEDERATION_STREAM_IDLE_TIMEOUT`].
pub(super) const FEDERATION_MAX_STREAM_FRAME_BYTES: usize = FEDERATION_MAX_RESPONSE_BYTES;
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

pub struct ServerHandle {
    _thread: JoinHandle<()>,
    /// Federation TCP accept thread, joined on drop. `None` when federation is
    /// not listening.
    federation_thread: Option<JoinHandle<()>>,
    /// Manager for outbound federation polls, proxy routes, and the inert-until-
    /// enabled saved-profile watcher. Shared with the app so config and catalog
    /// changes apply live. With no peer endpoint it drives zero poll threads and
    /// an empty registry, preserving local API behavior. Joined on drop.
    federation_manager: Arc<FederationPeerManager>,
    path: PathBuf,
    identity: SocketFileIdentity,
    running: Arc<AtomicBool>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);

        // The federation accept loop polls `running` between non-blocking
        // accepts, so clearing it above unblocks the thread; join it to release
        // the TCP listener before returning. (The unix accept thread is detached
        // like before — its listener drops with the process.)
        if let Some(thread) = self.federation_thread.take() {
            let _ = thread.join();
        }

        // The catalog watcher is stopped first. Outbound poll threads observe
        // the cleared `running` between short sleep increments; a mid-flight
        // poll is bounded by the request timeout. `join_all` also drains threads
        // still being retired by a concurrent reconcile.
        self.federation_manager.join_all();

        if let Err(err) = self.remove_socket_file_if_owned() {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.path.display(), err = %err, "failed to remove api socket on shutdown");
            }
        }
    }
}

impl ServerHandle {
    pub(crate) fn remove_socket_file_if_owned(&self) -> std::io::Result<()> {
        remove_socket_file_if_owned(&self.path, &self.identity)
    }

    /// The shared outbound federation peer manager. Handed to the `App` so its
    /// `reload-config` handler can reconcile the peer set live against the same
    /// manager this server boot-spawned and reads on the hot proxy path.
    pub fn federation_manager(&self) -> Arc<FederationPeerManager> {
        Arc::clone(&self.federation_manager)
    }
}

pub(crate) fn start_server_with_stop_control(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    server_stop: ServerStop,
    config: &crate::config::Config,
    federation_store: Arc<Mutex<FederationStore>>,
) -> std::io::Result<ServerHandle> {
    // Gram relay consent must be effective before the first peer reconcile
    // decides which saved peers get a reverse gateway.
    crate::api::gram_relay::apply_config(&config.gram_relay);
    let federation = &config.federation;
    start_server_inner(
        api_tx,
        event_hub,
        default_capabilities(),
        Some(server_stop),
        federation,
        federation_store,
    )
}

pub(crate) fn default_capabilities() -> Option<ServerCapabilities> {
    Some(ServerCapabilities {
        live_handoff: crate::platform::capabilities().live_handoff,
        detached_server_daemon: crate::platform::current_process_is_detached_server_daemon(),
        endpoint_protocol_generation: Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
        surface_interest: true,
        health_check: true,
        pane_input_stream: true,
        gram_upload_stream: true,
        agent_session_transfer: true,
        agent_session_transfer_harnesses: vec![
            crate::api::schema::AgentSessionTransferHarness::Claude,
            crate::api::schema::AgentSessionTransferHarness::Codex,
            crate::api::schema::AgentSessionTransferHarness::Omp,
        ],
        events_v2: true,
        ssh_agent_registration: false,
        agent_forget: true,
        api_bridge_multi: cfg!(unix),
        pane_history: true,
        agent_answer: true,
        agent_transcript: true,
    })
}

fn start_server_inner(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    mut capabilities: Option<ServerCapabilities>,
    server_stop: Option<ServerStop>,
    federation: &FederationConfig,
    federation_store: Arc<Mutex<FederationStore>>,
) -> std::io::Result<ServerHandle> {
    let path = socket_path();
    prepare_socket_path(&path)?;

    let listener = bind_local_listener(&path)?;
    restrict_socket_permissions(&path)?;
    let identity = socket_file_identity(&path)?;
    info!(path = %path.display(), "api server listening");

    #[cfg(unix)]
    let ssh_agents = match crate::platform::ssh_agent::SshAgentRegistry::new(
        crate::platform::ssh_agent::socket_path(),
        std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
    ) {
        Ok(registry) => Some(registry),
        Err(error) => {
            warn!(%error, "SSH agent refresh unavailable; retaining inherited pane environment");
            None
        }
    };

    if let Some(capabilities) = capabilities.as_mut() {
        capabilities.ssh_agent_registration = {
            #[cfg(unix)]
            {
                ssh_agents.is_some()
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
    }

    let running = Arc::new(AtomicBool::new(true));

    // Outbound federation peer manager: owns the per-peer poll threads and the
    // alias→target proxy registry (W4). Boot-spawns the configured peer set from
    // empty; when no peer has an `endpoint` this spawns zero threads and leaves
    // an empty registry, so the router never matches and local behavior is
    // byte-identical. Shared (as an `Arc`) with the `App` so `reload-config` can
    // add/remove/change peers live.
    let federation_manager = FederationPeerManager::new(
        Arc::clone(&federation_store),
        event_hub.clone(),
        Arc::clone(&running),
    );
    #[cfg(unix)]
    federation_manager.set_gram_api_sender(api_tx.clone());
    // Every session reads the machine-wide config; read the active session
    // once so the boot reconcile and the listener apply the same
    // default-session rule (`FederationConfig::for_session`).
    let session = crate::session::active_name();
    federation_manager.reconcile_config_for_session(federation, session.as_deref());
    #[cfg(unix)]
    guest_gate::install_context(api_tx.clone(), event_hub.clone(), Arc::clone(&running));

    let listener_running = Arc::clone(&running);
    let listener_api_tx = api_tx.clone();
    let listener_event_hub = event_hub.clone();
    let listener_capabilities = capabilities.clone();
    let listener_server_stop = server_stop.clone();
    let listener_federation_manager = Arc::clone(&federation_manager);
    let spawned = crate::thread_spawn::spawn_named("herdr-api-accept", move || {
        run_accept_loop(
            listener.incoming(),
            &listener_running,
            ACCEPT_ERROR_BACKOFF,
            |stream| {
                let api_tx = listener_api_tx.clone();
                let event_hub = listener_event_hub.clone();
                let capabilities = listener_capabilities.clone();
                let server_stop = listener_server_stop.clone();
                let connection_running = Arc::clone(&listener_running);
                let federation_manager = Arc::clone(&listener_federation_manager);
                #[cfg(unix)]
                let ssh_agents = ssh_agents.clone();
                spawn_connection_handler(move || {
                    // Snapshot the outbound proxy registry per connection so a
                    // concurrent reconcile (add/remove/change peer) is picked
                    // up without restarting the accept loop.
                    let federation_peers = federation_manager.registry_snapshot();
                    if let Err(err) = handle_connection_with_stop(
                        ApiStream::Local(stream),
                        &api_tx,
                        &event_hub,
                        &connection_running,
                        capabilities,
                        server_stop.as_ref(),
                        // Local unix-socket connections are never federation
                        // peers, so they bypass the capability-tier gate.
                        None,
                        &federation_peers,
                        #[cfg(unix)]
                        ssh_agents.as_ref(),
                    ) {
                        warn!(err = %err, "api connection failed");
                    }
                });
            },
        );
        debug!("api server thread exiting");
    });
    let thread = match spawned {
        Ok(thread) => thread,
        Err(err) => {
            // No ServerHandle owns the socket yet, so its Drop cleanup won't run.
            let _ = remove_socket_file_if_owned(&path, &identity);
            return Err(err);
        }
    };

    // A bad federation address or an address already in use must not crash the
    // daemon: `maybe_start_federation_listener` logs and returns `None`, leaving
    // the unix listener above serving on its own.
    // NB: the federation-inbound listener is deliberately NOT given the outbound
    // `federation_peers` registry. The home drives its OWN peers from local
    // clients only; an inbound peer must never be able to relay through the home
    // to its other peers (a confused-deputy trust expansion). Keeping the
    // registry out of this chain makes that unreachable by construction.
    let federation_thread = maybe_start_federation_listener(
        federation,
        session.as_deref(),
        &api_tx,
        &event_hub,
        &capabilities,
        &running,
        &server_stop,
    );

    // Outbound side is owned by `federation_manager`, boot-reconciled above from
    // explicit peers plus opt-in coordinator profiles. With neither source, it
    // drives zero poll threads and an empty registry, so local `agent.list`
    // behavior is unchanged.

    Ok(ServerHandle {
        _thread: thread,
        federation_thread,
        federation_manager,
        path,
        identity,
        running,
    })
}

/// Bind the federation TCP listener and spawn its accept thread when federation
/// is enabled and configured for `session`. Returns `None` (federation
/// disabled) for any non-fatal reason — disabled, a named session not in
/// `federation.named_sessions` (the default session owns `listen_addr`), no
/// address, no usable token, or a bind failure — after logging, so the daemon
/// keeps running on the unix socket alone.
fn maybe_start_federation_listener(
    federation: &FederationConfig,
    session: Option<&str>,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    capabilities: &Option<ServerCapabilities>,
    running: &Arc<AtomicBool>,
    server_stop: &Option<ServerStop>,
) -> Option<JoinHandle<()>> {
    let federation = federation.for_session(session);
    if !federation.listen {
        return None;
    }
    let Some(addr) = federation.listen_addr.as_deref() else {
        warn!("federation.listen is enabled but federation.listen_addr is unset; not binding");
        return None;
    };

    // Safety floor: never open a federation listener without at least one token
    // to authenticate against, or every peer would be admitted.
    let peers = resolve_federation_tokens(&federation);
    if peers.is_empty() {
        warn!(
            addr = %addr,
            "federation.listen is enabled but no peer token file yielded a token; \
             refusing to bind an unauthenticated federation listener"
        );
        return None;
    }

    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => {
            error!(addr = %addr, err = %err, "failed to bind federation listener; serving unix socket only");
            return None;
        }
    };
    let bound = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| addr.to_string());
    info!(addr = %bound, peer_tokens = peers.len(), "federation listener listening");

    match spawn_federation_listener(
        listener,
        peers,
        api_tx.clone(),
        event_hub.clone(),
        capabilities.clone(),
        Arc::clone(running),
        server_stop.clone(),
    ) {
        Ok(thread) => Some(thread),
        Err(err) => {
            error!(err = %err, "failed to start federation listener thread; serving unix socket only");
            None
        }
    }
}

/// Read each peer's `token_file`, trimmed, into the accepted set paired with the
/// peer's identity and capability tier. A single unreadable/empty token file is
/// logged and skipped rather than failing the listener; peers without a token
/// file are skipped (they have no inbound credential). The returned tier is the
/// authority a request authenticated by that token is bound to.
fn resolve_federation_tokens(federation: &FederationConfig) -> Vec<(String, PeerContext)> {
    let mut peers = Vec::new();
    for peer in &federation.peers {
        let Some(path) = peer.token_file.as_deref() else {
            continue;
        };
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let token = contents.trim();
                if token.is_empty() {
                    warn!(path = %path, alias = %peer.alias, "federation peer token file is empty; skipping");
                } else {
                    peers.push((
                        token.to_string(),
                        PeerContext {
                            alias: peer.alias.clone(),
                            tier: peer.capability,
                            expected_node_id: peer.expected_node_id.clone(),
                        },
                    ));
                }
            }
            Err(err) => {
                warn!(path = %path, alias = %peer.alias, err = %err, "failed to read federation peer token file; skipping");
            }
        }
    }
    peers
}

/// Spawn the federation TCP accept loop. Each accepted connection is handed to
/// [`handle_federation_connection`], which enforces the token gate before any
/// request is dispatched.
///
/// The listener is non-blocking and the loop polls `running` between accepts, so
/// [`ServerHandle::drop`] can stop and join it by clearing `running`.
#[allow(clippy::too_many_arguments)]
fn spawn_federation_listener(
    listener: TcpListener,
    peers: Vec<(String, PeerContext)>,
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    capabilities: Option<ServerCapabilities>,
    running: Arc<AtomicBool>,
    server_stop: Option<ServerStop>,
) -> std::io::Result<JoinHandle<()>> {
    listener.set_nonblocking(true)?;
    let peers = Arc::new(peers);
    let in_flight = Arc::new(AtomicUsize::new(0));

    let thread = std::thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, peer)) => {
                    // Refuse (and immediately close) connections beyond the cap so
                    // a peer cannot drive unbounded thread/fd growth. The accept
                    // loop is the only writer of `in_flight`, so this load/compare
                    // is race-free against itself; connection threads only ever
                    // decrement it via the RAII guard below.
                    if in_flight.load(Ordering::Acquire) >= MAX_FEDERATION_CONNECTIONS {
                        warn!(
                            peer = %peer,
                            max = MAX_FEDERATION_CONNECTIONS,
                            "federation connection cap reached; refusing connection"
                        );
                        drop(stream);
                        continue;
                    }
                    // The accepted socket is used in blocking mode; the framed
                    // reader toggles polling on it as needed.
                    if let Err(err) = stream.set_nonblocking(false) {
                        warn!(err = %err, "federation connection blocking-mode reset failed");
                        continue;
                    }
                    in_flight.fetch_add(1, Ordering::AcqRel);
                    // Released when the connection thread returns, including on an
                    // early return or panic, so a slot is never leaked.
                    let slot = ConnectionSlot {
                        in_flight: Arc::clone(&in_flight),
                    };
                    let peers = Arc::clone(&peers);
                    let api_tx = api_tx.clone();
                    let event_hub = event_hub.clone();
                    let capabilities = capabilities.clone();
                    let server_stop = server_stop.clone();
                    let connection_running = Arc::clone(&running);
                    std::thread::spawn(move || {
                        let _slot = slot;
                        if let Err(err) = handle_federation_connection(
                            ApiStream::Tcp(stream),
                            &peers,
                            &api_tx,
                            &event_hub,
                            &connection_running,
                            capabilities,
                            server_stop.as_ref(),
                        ) {
                            warn!(err = %err, peer = %peer, "federation connection failed");
                        }
                    });
                }
                Err(err) => match classify_accept_error(&err) {
                    // No connection pending: idle back-off, no logging.
                    AcceptBackoff::Idle => {
                        std::thread::sleep(CONNECTION_POLL_INTERVAL);
                    }
                    // A transient accept error (EMFILE from fd exhaustion,
                    // ECONNABORTED from a peer that vanished, ...) must not
                    // permanently kill the listener. Log, back off one poll
                    // interval, and keep serving; the loop still exits when
                    // `running` is cleared. There is deliberately no fatal path.
                    AcceptBackoff::Retry => {
                        warn!(err = %err, "federation listener accept error; continuing");
                        std::thread::sleep(CONNECTION_POLL_INTERVAL);
                    }
                },
            }
        }
        debug!("federation listener thread exiting");
    });

    Ok(thread)
}

/// RAII slot for the federation in-flight connection counter. Incremented before
/// a connection thread is spawned and decremented when that thread returns, so a
/// slot is released even on an early return or a panic.
struct ConnectionSlot {
    in_flight: Arc<AtomicUsize>,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// How the federation accept loop reacts to a `TcpListener::accept()` result that
/// is not a fresh connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceptBackoff {
    /// No connection was pending (`WouldBlock` on the non-blocking listener).
    Idle,
    /// A transient accept error: back off and keep serving.
    Retry,
}

/// Classify an accept error. `WouldBlock` is the ordinary "nothing pending"
/// signal; every other error is treated as transient and retried. There is
/// deliberately no fatal classification — a transient accept failure (EMFILE,
/// ECONNABORTED, ...) must never permanently kill the federation listener.
fn classify_accept_error(err: &io::Error) -> AcceptBackoff {
    if err.kind() == io::ErrorKind::WouldBlock {
        AcceptBackoff::Idle
    } else {
        AcceptBackoff::Retry
    }
}

/// Poll one peer forever, updating `cache` after each attempt. Resolves the
/// endpoint and token once up front; a bad endpoint logs and exits the thread
/// rather than spinning.
///
/// The thread runs until EITHER the global `running` flag clears (daemon
/// shutdown) OR this peer's own `peer_stop` flag is set (the manager retiring
/// this peer on a `reload-config` reconcile). `peer_stop` is per-peer so one
/// removed peer can be stopped without disturbing the others.
pub(crate) fn run_federation_peer_poll(
    peer: FederationPeer,
    route: PeerRoute,
    presentation: Arc<RwLock<PeerPresentation>>,
    cache: Arc<Mutex<FederationStore>>,
    running: Arc<AtomicBool>,
    peer_stop: Arc<AtomicBool>,
) {
    let client = route.client();
    let mut tracker = ReachabilityTracker::default();
    let mut workspace_refresh = WorkspaceRefresh::default();

    while running.load(Ordering::Relaxed) && !peer_stop.load(Ordering::Relaxed) {
        let reachability = poll_once_into_cache(
            &client,
            &peer.alias,
            peer.expected_node_id.as_deref(),
            Some(&route),
            &presentation,
            &cache,
            &mut tracker,
            &running,
            &peer_stop,
        );
        if reachability == Reachability::Reachable {
            refresh_workspaces_into_cache(
                &client,
                &peer.alias,
                &presentation,
                &cache,
                &mut workspace_refresh,
                Instant::now(),
                &running,
                &peer_stop,
            );
        }
        let interval = if reachability == Reachability::Reachable {
            FEDERATION_POLL_INTERVAL
        } else {
            failure_backoff(FEDERATION_POLL_INTERVAL)
        };
        sleep_interruptible(&running, &peer_stop, interval);
    }
    debug!(alias = %peer.alias, "federation peer poll thread exiting");
}

/// Schedule for the slower `workspace.list` fetch that rides on the agent poll.
///
/// Workspaces change far less often than agent status, so a reachable peer's
/// workspaces are fetched on the first successful agent poll, then every
/// [`FEDERATION_WORKSPACE_POLL_INTERVAL`]; the agent poll keeps its 5 s cadence
/// and adds no request in between. One early fetch is allowed when a polled
/// agent names a workspace the cache does not have (a workspace created on the
/// peer), bounded to ids that were not already missing after the last fetch so
/// an id the peer does not list cannot turn every agent poll into a fetch.
#[derive(Debug, Default)]
struct WorkspaceRefresh {
    last_attempt: Option<Instant>,
    missing_after_last_attempt: HashSet<String>,
}

impl WorkspaceRefresh {
    fn due(&self, now: Instant, missing: &HashSet<String>) -> bool {
        let Some(last_attempt) = self.last_attempt else {
            return true;
        };
        now.saturating_duration_since(last_attempt) >= FEDERATION_WORKSPACE_POLL_INTERVAL
            || !missing.is_subset(&self.missing_after_last_attempt)
    }
}

/// Fetch the peer's `workspace.list` into `cache` when [`WorkspaceRefresh`]
/// says it is due. Called only after a successful agent poll, which owns the
/// peer's reachability: a failed workspace fetch keeps the last-known
/// workspaces, logs, and waits for the next slow tick instead of retrying on
/// every agent poll.
#[allow(clippy::too_many_arguments)]
fn refresh_workspaces_into_cache(
    client: &ApiClient,
    alias: &str,
    presentation: &Arc<RwLock<PeerPresentation>>,
    cache: &Mutex<FederationStore>,
    refresh: &mut WorkspaceRefresh,
    now: Instant,
    running: &Arc<AtomicBool>,
    peer_stop: &Arc<AtomicBool>,
) {
    let missing = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .missing_workspace_ids(alias);
    if !refresh.due(now, &missing) {
        return;
    }
    refresh.last_attempt = Some(now);
    let workspaces = match poll_peer_workspace_list(client, running) {
        Ok(workspaces) => workspaces,
        Err(err) => {
            warn!(alias = %alias, err = %err, "federation peer workspace poll failed");
            refresh.missing_after_last_attempt = missing;
            return;
        }
    };
    let presentation = presentation
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut rejected = 0usize;
    let qualified = workspaces
        .into_iter()
        .filter_map(|workspace| {
            let workspace = qualify_remote_workspace(alias, &presentation, workspace);
            rejected += usize::from(workspace.is_none());
            workspace
        })
        .collect();
    if rejected != 0 {
        warn!(
            alias = %alias,
            rejected,
            "federation peer returned already-qualified or invalid workspace identities"
        );
    }
    let mut store = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Same under-lock stop guard as `poll_once_into_cache`.
    if peer_stop.load(Ordering::Relaxed) {
        return;
    }
    store.set_peer_workspaces(alias, qualified);
    refresh.missing_after_last_attempt = store.missing_workspace_ids(alias);
}

/// Fetch one peer's own workspaces with `workspace.list{local_only:true}`, so a
/// peer that is itself a coordinator never re-exports its peers' workspaces.
/// Bounded exactly like [`poll_peer_agent_list`].
fn poll_peer_workspace_list(
    client: &ApiClient,
    running: &Arc<AtomicBool>,
) -> Result<Vec<crate::api::schema::WorkspaceInfo>, ApiClientError> {
    let request = Request {
        id: "federation:workspace.list".into(),
        method: Method::WorkspaceList(crate::api::schema::WorkspaceListParams { local_only: true }),
    };
    let value = client.request_value_bounded(
        &request,
        FEDERATION_MAX_RESPONSE_BYTES,
        FEDERATION_POLL_REQUEST_TIMEOUT,
        Some(running),
    )?;
    match parse_response_value(value)?.result {
        ResponseResult::WorkspaceList { mut workspaces } => {
            // Fail-safe for a peer that ignores `local_only`: a local workspace
            // never carries federation machine identity.
            workspaces.retain(|workspace| workspace.machine_id.is_none());
            Ok(workspaces)
        }
        other => Err(ApiClientError::UnexpectedResult(format!("{other:?}"))),
    }
}

pub(super) struct PeerPollSnapshot {
    pub(super) agents: Vec<crate::api::schema::AgentInfo>,
    pub(super) remote_boot_id: Option<String>,
    observation: PeerObservation,
}

fn federation_poll_error_class(error: &ApiClientError) -> FederationPollErrorClass {
    match error {
        ApiClientError::ErrorResponse(response) if response.error.code == "unauthorized" => {
            FederationPollErrorClass::AuthenticationFailed
        }
        ApiClientError::UnexpectedResult(message)
            if message.starts_with("peer machine identity mismatch:") =>
        {
            FederationPollErrorClass::IdentityMismatch
        }
        ApiClientError::Io(_) | ApiClientError::EmptyResponse => {
            FederationPollErrorClass::Transport
        }
        ApiClientError::Json(_)
        | ApiClientError::ErrorResponse(_)
        | ApiClientError::UnexpectedResult(_) => FederationPollErrorClass::Protocol,
    }
}
/// Run one poll of `client`'s `agent.list` and fold the result into `cache`:
/// on success, alias-prefix the agents and store them `Reachable`; on failure,
/// advance the miss tracker and degrade the peer (retaining last-known agents)
/// without dropping the entry. Returns the resulting reachability so the caller
/// can pick its next sleep. Factored out of the poll loop so the miss→degrade→
/// unreachable path is testable without waiting on real poll intervals.
fn poll_once_into_cache(
    client: &ApiClient,
    alias: &str,
    expected_machine_id: Option<&str>,
    route: Option<&PeerRoute>,
    presentation: &Arc<RwLock<PeerPresentation>>,
    cache: &Mutex<FederationStore>,
    tracker: &mut ReachabilityTracker,
    running: &Arc<AtomicBool>,
    peer_stop: &Arc<AtomicBool>,
) -> Reachability {
    // Taken before the request: any status the event relay publishes after
    // this instant is newer than this poll's answer and wins over it.
    let poll_started = Instant::now();
    match poll_peer_agent_list(client, running, expected_machine_id) {
        Ok(snapshot) => {
            if let Some(route) = route {
                route.observe_remote_boot(snapshot.remote_boot_id.as_deref());
                route.observe_api_bridge_multi(
                    snapshot
                        .observation
                        .remote_capabilities
                        .as_ref()
                        .is_some_and(|capabilities| capabilities.api_bridge_multi),
                );
            }
            let mut rejected = 0usize;
            let presentation = presentation
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let prefixed = snapshot
                .agents
                .into_iter()
                .filter_map(|agent| {
                    let agent = prefix_remote_agent(alias, &presentation, agent);
                    rejected += usize::from(agent.is_none());
                    agent
                })
                .collect();
            if rejected != 0 {
                warn!(
                    alias = %alias,
                    rejected,
                    "federation peer returned already-qualified or invalid agent identities"
                );
            }
            let reachability = tracker.record_success();
            let mut store = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Race guard for a changed peer: the manager sets this peer's
            // `peer_stop` and then evicts the alias while holding this same
            // store Mutex, so checking `peer_stop` here — WHILE HOLDING the lock, just
            // before the write — serializes a retiring thread against the
            // reconcile. If stop is set, skip the write so a stale entry can
            // never reappear after the alias was evicted (or be overwritten by
            // the newly spawned thread for the same alias). The store Mutex is
            // the happens-before edge between the two, so `Relaxed` is correct.
            if peer_stop.load(Ordering::Relaxed) {
                return reachability;
            }
            if let Some(route) = route {
                route.set_identity_validated(true);
            }
            store.set_polled_peer(
                alias.to_string(),
                PeerCacheEntry::reachable_observed(prefixed, Instant::now(), snapshot.observation),
                poll_started,
            );
            drop(presentation);
            reachability
        }
        Err(err) => {
            let error_class = federation_poll_error_class(&err);
            // Only a different machine behind the endpoint revokes its
            // identity. A transport, protocol or authentication miss says
            // nothing about who answers there, so the identity the last poll
            // proved stands and the event relay keeps streaming.
            if error_class == FederationPollErrorClass::IdentityMismatch {
                if let Some(route) = route {
                    route.set_identity_validated(false);
                }
            }
            let reachability = tracker.record_miss();
            warn!(alias = %alias, ?error_class, ?reachability, "federation peer poll failed");
            let mut store = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Same under-lock stop guard as the success arm: never degrade an
            // alias the reconcile has already evicted (the Mutex is the
            // happens-before edge, so `Relaxed` is correct).
            if peer_stop.load(Ordering::Relaxed) {
                return reachability;
            }
            store.degrade_peer(alias, reachability);
            store.record_poll_error(alias, error_class);
            reachability
        }
    }
}

/// Read a peer's shared token from its `token_file`, trimmed. Mirrors the inbound
/// [`resolve_federation_tokens`] read logic. `None` when there is no token file
/// or it is unreadable/empty; the outbound connection then sends no
/// `federation.hello` credential.
pub(crate) fn read_peer_token(peer: &FederationPeer) -> Option<String> {
    let path = peer.token_file.as_deref()?;
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let token = contents.trim();
            if token.is_empty() {
                warn!(path = %path, alias = %peer.alias, "federation peer token file is empty; polling without a token");
                None
            } else {
                Some(token.to_string())
            }
        }
        Err(err) => {
            warn!(path = %path, alias = %peer.alias, err = %err, "failed to read federation peer token file; polling without a token");
            None
        }
    }
}

/// Poll one peer's `agent.list` and return its agents. A transport error, a
/// timeout, an error response, or an unexpected result all surface as `Err` and
/// count as a miss.
///
/// The response read is BOTH byte-bounded ([`FEDERATION_MAX_RESPONSE_BYTES`]) and
/// total-time-bounded ([`FEDERATION_POLL_REQUEST_TIMEOUT`], a wall-clock deadline)
/// so a malicious peer cannot drive unbounded allocation or an unbounded-time
/// read; `running` lets the read abort promptly on shutdown so [`ServerHandle`]'s
/// drop, which joins the poll threads, does not hang.
pub(super) fn poll_peer_agent_list(
    client: &ApiClient,
    running: &Arc<AtomicBool>,
    expected_machine_id: Option<&str>,
) -> Result<PeerPollSnapshot, ApiClientError> {
    let request = Request {
        id: "federation:agent.list".into(),
        method: Method::AgentList(crate::api::schema::AgentListParams { local_only: true }),
    };
    let value = client.request_value_bounded(
        &request,
        FEDERATION_MAX_RESPONSE_BYTES,
        FEDERATION_POLL_REQUEST_TIMEOUT,
        Some(running),
    )?;
    let response = parse_response_value(value)?;
    match response.result {
        ResponseResult::AgentList {
            mut agents,
            origin_machine_id,
            origin_boot_id,
            origin_version,
            origin_protocol,
            origin_capabilities,
        } => {
            if let Some(expected) = expected_machine_id {
                if origin_machine_id.as_deref() != Some(expected) {
                    return Err(ApiClientError::UnexpectedResult(format!(
                        "peer machine identity mismatch: expected {expected:?}, received {:?}",
                        origin_machine_id.as_deref()
                    )));
                }
            }
            // Older peers ignore the new local_only parameter because their
            // empty-params schema accepts unknown fields. Filter their aggregate
            // response as a fail-safe: a local agent never has federation-owned
            // machine identity, while every cached remote agent does.
            agents.retain(|agent| agent.machine_id.is_none());
            // The response-level value is the authoritative peer report. Clear
            // any per-agent claim from mixed or malicious peers, then stamp the
            // validated identity consistently across this snapshot.
            for agent in &mut agents {
                agent.origin_machine_id.clone_from(&origin_machine_id);
            }
            Ok(PeerPollSnapshot {
                agents,
                remote_boot_id: origin_boot_id.clone(),
                observation: PeerObservation {
                    validated_machine_id: origin_machine_id,
                    remote_boot_id: origin_boot_id,
                    remote_version: origin_version,
                    remote_protocol: origin_protocol,
                    remote_capabilities: origin_capabilities,
                },
            })
        }
        other => Err(ApiClientError::UnexpectedResult(format!("{other:?}"))),
    }
}

/// Rewrite a remote agent's identity fields so it is unambiguous once merged into
/// the local `agent.list`: `name` and all four id fields gain exactly one
/// `<alias>/` prefix, `machine_id` retains the compatibility routing alias, and
/// `machine_label` carries its display value.
///
/// The home owns the routing alias, saved-profile namespace, display label, and
/// reachability fields. `poll_peer_agent_list` has already replaced every
/// per-agent origin claim with the response-level machine identity and checked
/// an optional pin. An already-qualified target is rejected rather than prefixed
/// twice. [`FederationStore::merged_agents`] is the only code that sets
/// `reachability`/`last_known_status`, from the home's poll-outcome tracking.
pub(crate) fn prefix_remote_agent(
    alias: &str,
    presentation: &PeerPresentation,
    mut agent: crate::api::schema::AgentInfo,
) -> Option<crate::api::schema::AgentInfo> {
    let qualify = qualify_remote_id;
    let archived = agent.archived.is_some();
    agent.name = agent.name.map(|name| format!("{alias}/{name}"));
    agent.terminal_id = qualify(alias, agent.terminal_id, false)?;
    agent.workspace_id = qualify(alias, agent.workspace_id, archived)?;
    agent.tab_id = qualify(alias, agent.tab_id, archived)?;
    agent.pane_id = qualify(alias, agent.pane_id, archived)?;
    // Home-owned federation fields. The legacy `machine_id` remains the routing
    // alias for wire compatibility. Explicit peers have no saved profile id;
    // `origin_machine_id` is the response-level identity validated by the poll.
    agent.machine_id = Some(alias.to_string());
    agent
        .machine_profile_id
        .clone_from(&presentation.profile_id);
    agent.machine_label = Some(presentation.label.clone());
    agent.reachability = None;
    agent.last_known_status = None;
    Some(agent)
}

/// Give one peer-local id exactly one `<alias>/` prefix. An empty id is kept
/// empty when `allow_empty`; an id that already contains `/` (already
/// qualified, or a malformed peer) is rejected rather than prefixed twice.
pub(super) fn qualify_remote_id(alias: &str, value: String, allow_empty: bool) -> Option<String> {
    if value.is_empty() {
        return allow_empty.then_some(value);
    }
    if value.contains('/') {
        return None;
    }
    Some(format!("{alias}/{value}"))
}

/// Rewrite a peer's workspace for the home's directory: `workspace_id` and
/// `active_tab_id` gain one `<alias>/` prefix and the home-owned machine fields
/// are stamped. `number`, `label`, `focused` and position are the peer's own so
/// a client can reproduce that machine's sidebar. `reachability` and
/// `last_known_status` are set only by [`FederationStore::merged_workspaces`].
pub(super) fn qualify_remote_workspace(
    alias: &str,
    presentation: &PeerPresentation,
    mut workspace: crate::api::schema::WorkspaceInfo,
) -> Option<crate::api::schema::WorkspaceInfo> {
    workspace.workspace_id = qualify_remote_id(alias, workspace.workspace_id, false)?;
    workspace.active_tab_id = qualify_remote_id(alias, workspace.active_tab_id, true)?;
    workspace.machine_id = Some(alias.to_owned());
    workspace
        .machine_profile_id
        .clone_from(&presentation.profile_id);
    workspace.machine_label = Some(presentation.label.clone());
    workspace.reachability = None;
    workspace.last_known_status = None;
    Some(workspace)
}

/// Rewrite a peer's tab: `tab_id` and `workspace_id` gain one `<alias>/` prefix
/// and the home-owned machine fields are stamped.
pub(super) fn qualify_remote_tab(
    alias: &str,
    presentation: &PeerPresentation,
    mut tab: crate::api::schema::TabInfo,
) -> Option<crate::api::schema::TabInfo> {
    tab.tab_id = qualify_remote_id(alias, tab.tab_id, false)?;
    tab.workspace_id = qualify_remote_id(alias, tab.workspace_id, false)?;
    tab.machine_id = Some(alias.to_owned());
    tab.machine_profile_id.clone_from(&presentation.profile_id);
    tab.machine_label = Some(presentation.label.clone());
    Some(tab)
}

/// Qualify the ids in a routed `workspace.get`/`tab.get`/`tab.list` reply so the
/// client keeps seeing federated identities it can route again. Every other
/// line (errors, other methods' results) passes through verbatim. A peer reply
/// whose ids are already qualified is refused as `peer_invalid_response`
/// instead of being prefixed twice.
/// A peer older than `agent.transcript` cannot parse the request and answers
/// `invalid_request`; the caller learns the peer has no transcripts instead.
fn unknown_transcript_as_unsupported(alias: &str, line: String) -> String {
    match serde_json::from_str::<ErrorResponse>(&line) {
        Ok(error) if error.error.code == "invalid_request" => error_response_json(
            error.id,
            "transcript_unsupported",
            format!("machine {alias} does not serve agent transcripts"),
        ),
        _ => line,
    }
}

fn qualify_routed_response(alias: &str, presentation: &PeerPresentation, line: String) -> String {
    let Ok(response) = serde_json::from_str::<SuccessResponse>(&line) else {
        return line;
    };
    let SuccessResponse { id, result } = response;
    let result = match result {
        ResponseResult::WorkspaceInfo { workspace } => {
            qualify_remote_workspace(alias, presentation, workspace)
                .map(|workspace| ResponseResult::WorkspaceInfo { workspace })
        }
        ResponseResult::TabInfo { tab } => {
            qualify_remote_tab(alias, presentation, tab).map(|tab| ResponseResult::TabInfo { tab })
        }
        ResponseResult::TabList { tabs } => tabs
            .into_iter()
            .map(|tab| qualify_remote_tab(alias, presentation, tab))
            .collect::<Option<Vec<_>>>()
            .map(|tabs| ResponseResult::TabList { tabs }),
        _ => return line,
    };
    match result {
        Some(result) => {
            serde_json::to_string(&SuccessResponse { id, result }).unwrap_or_else(|_| line.clone())
        }
        None => error_response_json(
            id,
            "peer_invalid_response",
            format!("federation peer {alias:?} returned already-qualified ids"),
        ),
    }
}

/// Sleep up to `total`, waking every [`FEDERATION_POLL_STEP`] to re-check the
/// stop flags, so neither a shutdown nor a `reload-config` peer retirement is
/// delayed by the poll interval. Returns early the moment EITHER `running`
/// clears (daemon shutdown) OR `peer_stop` is set (this peer removed/changed).
pub(super) fn sleep_interruptible(
    running: &Arc<AtomicBool>,
    peer_stop: &Arc<AtomicBool>,
    total: Duration,
) {
    let mut elapsed = Duration::ZERO;
    while elapsed < total {
        if !running.load(Ordering::Relaxed) || peer_stop.load(Ordering::Relaxed) {
            return;
        }
        let step = FEDERATION_POLL_STEP.min(total - elapsed);
        std::thread::sleep(step);
        elapsed += step;
    }
}

/// Base poll interval plus a small randomized jitter, so peers that fail at the
/// same time do not retry in lockstep. Jitter is a cheap wall-clock-derived
/// value — no RNG dependency — capped at [`FEDERATION_POLL_MAX_JITTER`].
pub(crate) fn failure_backoff(base: Duration) -> Duration {
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| u64::from(since.subsec_nanos()))
        .unwrap_or(0);
    let cap = FEDERATION_POLL_MAX_JITTER
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64;
    let jitter = if cap == 0 { 0 } else { entropy % cap };
    base + Duration::from_nanos(jitter)
}
/// Enforce the federation handshake, then dispatch the connection normally.
///
/// The first line MUST be a `federation.hello` whose `proto_version` this daemon
/// speaks and whose token matches one configured peer (constant-time compared).
/// A malformed/missing hello or an unknown token is rejected as `unauthorized`;
/// a version the daemon does not speak is rejected as `federation_protocol_mismatch`.
/// Either way a single JSON error line is written and the connection closes with
/// no request dispatched. On success the connection is bound to the matched
/// peer's [`PeerContext`] and the remaining lines flow through the same
/// [`handle_connection_with_stop`] path as a local connection — but gated to the
/// peer's capability tier.
#[allow(clippy::too_many_arguments)]
fn handle_federation_connection(
    mut stream: ApiStream,
    peers: &[(String, PeerContext)],
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    capabilities: Option<ServerCapabilities>,
    server_stop: Option<&ServerStop>,
) -> std::io::Result<()> {
    if let Err(err) = stream.set_send_timeout(Some(STREAM_WRITE_TIMEOUT)) {
        debug!(err = %err, "federation connection write timeout unavailable");
    }

    let Some(line) = read_initial_request_line(&mut stream)? else {
        return Ok(()); // peer closed before sending the hello
    };

    let Some(hello) = FederationHello::from_line(line.trim()) else {
        // Missing or malformed hello: never reveals whether a token would match.
        write_json_line_allow_disconnect(&mut stream, &federation_unauthorized_error())?;
        return Ok(());
    };

    if hello.proto_version != FEDERATION_PROTOCOL_VERSION {
        write_json_line_allow_disconnect(&mut stream, &federation_protocol_mismatch_error())?;
        return Ok(());
    }

    let Some(peer) = authorized_peer(&hello.token, peers) else {
        write_json_line_allow_disconnect(&mut stream, &federation_unauthorized_error())?;
        return Ok(());
    };

    // Optional identity pin: when this peer is configured with an
    // `expected_node_id`, the install id presented in the hello must match it.
    // The token stays the authenticator; this only ADDS a bind when configured
    // (a peer with no pin is unaffected). Reuse the same opaque unauthorized line
    // as a token failure so a mismatch never reveals which check failed, and
    // reject here — before any request is dispatched.
    if let Some(expected) = peer.expected_node_id.as_deref() {
        if hello.machine_id != expected {
            write_json_line_allow_disconnect(&mut stream, &federation_unauthorized_error())?;
            return Ok(());
        }
    }

    // A federation-INBOUND connection must never drive the home's OWN outbound
    // peer routing. The home drives its peers; it does not RELAY an inbound peer
    // through to its other peers. If the inbound registry were passed here, an
    // inbound peer sending `<other-alias>/…` would be transit-proxied to that
    // other peer using the HOME's credentials/tier — a confused deputy that
    // expands the inbound peer's trust to peers it has no relationship with.
    // Pass an empty registry so `maybe_route_to_peer` never matches on this
    // path and a `<alias>/…` target falls through to a local not-found. This
    // single choke point also keeps the pane.stream proxy inbound-safe, since it
    // reads the same registry.
    let no_outbound_routing: HashMap<String, PeerRoute> = HashMap::new();
    handle_connection_with_stop(
        stream,
        api_tx,
        event_hub,
        running,
        capabilities,
        server_stop,
        Some(peer),
        &no_outbound_routing,
        // Federation peers may not register SSH agents (`server.ssh_agent.register`
        // is denied in the audit table), so inbound connections get no registry.
        #[cfg(unix)]
        None,
    )
}

fn federation_unauthorized_error() -> ErrorResponse {
    ErrorResponse {
        id: String::new(),
        error: ErrorBody {
            code: "unauthorized".into(),
            message: "federation authentication failed".into(),
        },
    }
}

/// Rejection line for a hello whose handshake version this daemon does not speak.
fn federation_protocol_mismatch_error() -> ErrorResponse {
    ErrorResponse {
        id: String::new(),
        error: ErrorBody {
            code: "federation_protocol_mismatch".into(),
            message: format!(
                "unsupported federation protocol version; this daemon speaks version {FEDERATION_PROTOCOL_VERSION}"
            ),
        },
    }
}

/// Rejection line for a federated request the peer's capability tier does not
/// permit (an out-of-tier or entirely denied method). Reuses the
/// [`federation_unauthorized_error`] error shape, but carries the request id so
/// the client can correlate it, and a `forbidden` code to distinguish an
/// authenticated-but-insufficient peer from a failed authentication.
fn federation_forbidden_error(id: String, method: &str) -> ErrorResponse {
    ErrorResponse {
        id,
        error: ErrorBody {
            code: "forbidden".into(),
            message: format!("federation peer is not permitted to call '{method}'"),
        },
    }
}

fn federation_identity_unverified_error(id: String, alias: &str) -> ErrorResponse {
    ErrorResponse {
        id,
        error: ErrorBody {
            code: "federation_identity_unverified".into(),
            message: format!("federation peer {alias:?} has not validated its machine identity"),
        },
    }
}

/// Starts one connection's worker. When the OS refuses a thread, only this
/// connection is dropped; the accept loop keeps serving.
fn spawn_connection_handler(work: impl FnOnce() + Send + 'static) {
    if let Err(err) = crate::thread_spawn::spawn_named("herdr-api-conn", work) {
        warn!(err = %err, "failed to spawn api connection thread; dropping connection");
    }
}

fn run_accept_loop<S>(
    incoming: impl IntoIterator<Item = io::Result<S>>,
    running: &AtomicBool,
    error_backoff: Duration,
    mut handle: impl FnMut(S),
) {
    let mut consecutive_errors = 0_u64;
    for stream in incoming {
        match stream {
            Ok(stream) => {
                if consecutive_errors > 0 {
                    info!(consecutive_errors, "api listener accept recovered");
                    consecutive_errors = 0;
                }
                handle(stream);
            }
            Err(err) => {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                // Accept errors such as ECONNABORTED or EMFILE are transient;
                // exiting would leave the socket file with no listener.
                if consecutive_errors == 0 {
                    error!(err = %err, "api listener accept failed; retrying");
                }
                consecutive_errors = consecutive_errors.saturating_add(1);
                std::thread::sleep(error_backoff);
                if !running.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod accept_loop_tests {
    use super::*;

    fn accept_error() -> io::Result<u32> {
        Err(io::Error::other("transient accept failure"))
    }

    #[test]
    fn keeps_serving_after_repeated_errors() {
        let running = AtomicBool::new(true);
        let mut handled = Vec::new();

        run_accept_loop(
            [Ok(1), accept_error(), accept_error(), Ok(2)],
            &running,
            Duration::ZERO,
            |stream| handled.push(stream),
        );

        assert_eq!(handled, vec![1, 2]);
    }

    #[test]
    fn keeps_serving_after_connection_thread_spawn_fails() {
        let running = AtomicBool::new(true);
        let (tx, rx) = std::sync::mpsc::channel();

        crate::thread_spawn::test_hook::fail_next_spawns(1);
        run_accept_loop([Ok(1), Ok(2)], &running, Duration::ZERO, |stream| {
            let tx = tx.clone();
            spawn_connection_handler(move || tx.send(stream).unwrap());
        });
        drop(tx);

        assert_eq!(rx.iter().collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn exits_when_shutdown_happens_while_errors_continue() {
        let running = AtomicBool::new(true);
        let mut attempts = 0;
        let incoming = std::iter::from_fn(|| {
            attempts += 1;
            if attempts == 3 {
                running.store(false, Ordering::Relaxed);
            }
            Some(accept_error())
        });

        run_accept_loop(incoming, &running, Duration::ZERO, |_| {
            panic!("no connection should be handled")
        });

        assert_eq!(attempts, 3);
    }
}

fn retired_pane_graphics_method_error(line: &str, id: &str) -> Option<ErrorResponse> {
    #[derive(serde::Deserialize)]
    struct RequestMethod {
        method: String,
    }

    let envelope = serde_json::from_str::<RequestMethod>(line).ok()?;
    let method = envelope.method.as_str();
    if !matches!(
        method,
        "pane.graphics.info" | "pane.graphics.set" | "pane.graphics.clear" | "pane.graphics.stream"
    ) {
        return None;
    }

    Some(ErrorResponse {
        id: id.into(),
        error: ErrorBody {
            code: "unknown_method".into(),
            message: format!("unknown method: {method}"),
        },
    })
}
fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    crate::ipc::prepare_socket_path(path, |path| {
        format!(
            "herdr is already running (socket busy at {})",
            path.display()
        )
    })
}

fn restrict_socket_permissions(path: &Path) -> std::io::Result<()> {
    crate::ipc::restrict_socket_permissions(path, SOCKET_PERMISSION_MODE)
}

#[cfg(test)]
fn handle_connection(
    stream: LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    capabilities: Option<ServerCapabilities>,
) -> std::io::Result<()> {
    handle_connection_with_stop(
        ApiStream::Local(stream),
        api_tx,
        event_hub,
        running,
        capabilities,
        None,
        None,
        &HashMap::new(),
        #[cfg(unix)]
        None,
    )
}

/// Who is on the other end of one API connection.
// One per connection and moved, not stored; the guest variant stays unboxed
// like `crate::guest::Admission`.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ConnectionPrincipal {
    /// The local socket or an SSH bridge to it: the owner.
    Owner,
    /// An inbound federation TCP peer, gated by its capability tier.
    Federation(PeerContext),
    /// A HerdrUp guest through the guest relay, gated to one agent.
    #[cfg(unix)]
    Guest(crate::guest::GuestPrincipal),
}

#[allow(clippy::too_many_arguments)]
fn handle_connection_with_stop(
    stream: ApiStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    capabilities: Option<ServerCapabilities>,
    server_stop: Option<&ServerStop>,
    federation: Option<PeerContext>,
    federation_peers: &HashMap<String, PeerRoute>,
    #[cfg(unix)] ssh_agents: Option<&crate::platform::ssh_agent::SshAgentRegistry>,
) -> std::io::Result<()> {
    let principal = match federation {
        Some(peer) => ConnectionPrincipal::Federation(peer),
        None => ConnectionPrincipal::Owner,
    };
    handle_principal_connection(
        stream,
        api_tx,
        event_hub,
        running,
        capabilities,
        server_stop,
        principal,
        federation_peers,
        #[cfg(unix)]
        ssh_agents,
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_principal_connection(
    mut stream: ApiStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    capabilities: Option<ServerCapabilities>,
    server_stop: Option<&ServerStop>,
    principal: ConnectionPrincipal,
    federation_peers: &HashMap<String, PeerRoute>,
    #[cfg(unix)] ssh_agents: Option<&crate::platform::ssh_agent::SshAgentRegistry>,
) -> std::io::Result<()> {
    if let Err(err) = stream.set_send_timeout(Some(STREAM_WRITE_TIMEOUT)) {
        debug!(err = %err, "api connection write timeout unavailable");
    }

    let Some(line) = read_initial_request_line(&mut stream)? else {
        return Ok(());
    };

    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let mut request = match serde_json::from_str::<Request>(line) {
        Ok(request) => request,
        Err(request_error) => {
            // Recover correlation without relaxing typed request validation or accepting
            // ambiguous duplicate IDs. Invalid JSON and non-string IDs stay uncorrelated.
            #[derive(serde::Deserialize)]
            struct RequestId {
                id: String,
            }
            let id = if line.starts_with('{') {
                serde_json::from_str::<RequestId>(line)
                    .map(|request| request.id)
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let response =
                retired_pane_graphics_method_error(line, &id).unwrap_or_else(|| ErrorResponse {
                    id,
                    error: ErrorBody {
                        code: "invalid_request".into(),
                        message: format!("invalid request: {request_error}"),
                    },
                });
            write_json_line_allow_disconnect(&mut stream, &response)?;
            return Ok(());
        }
    };

    let request_id = request.id.clone();
    let method = api_method_name(&request.method);
    let changes_ui = request_changes_ui(&request);

    // A guest never reaches the owner or federation paths below: its gate is
    // an explicit allowlist bound to one agent.
    #[cfg(unix)]
    if let ConnectionPrincipal::Guest(guest) = &principal {
        return guest_gate::serve_request(stream, request, guest, api_tx, event_hub, running);
    }
    let federation = match &principal {
        ConnectionPrincipal::Federation(peer) => Some(peer),
        _ => None,
    };

    // Federation capability gate. Local (unix-socket) connections carry `None`
    // here and are never filtered; only a connection bound to a federation
    // `PeerContext` is checked. A method that is denied to federation entirely,
    // or that requires a higher tier than this peer holds, is refused before any
    // dispatch or stream is set up.
    if let Some(peer) = &federation {
        let permitted = matches!(
            federation_access(method),
            FederationAccess::AllowedAt(required) if required <= peer.tier
        );
        if !permitted {
            warn!(
                method,
                alias = %peer.alias,
                "federation request denied by capability tier"
            );
            crate::logging::api_request_started(&request_id, method, changes_ui);
            let response = federation_forbidden_error(request_id.clone(), method);
            let result = write_json_line_allow_disconnect(&mut stream, &response);
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "forbidden",
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            return result;
        }
    }

    crate::logging::api_request_started(&request_id, method, changes_ui);
    // Remote Gram is opt-in on the remote daemon too. Its local clients keep
    // using the ordinary Gram API, but requests go to the coordinator's
    // restricted reverse SSH socket instead of a second local store. The
    // gateway stamps the pinned machine alias; TCP federation never enters here.
    #[cfg(unix)]
    if federation.is_none() {
        if let Some(response) = crate::api::reverse::forward_local(&request) {
            let written = write_text_line_allow_disconnect(&mut stream, &response);
            // The coordinator keeps the Gram; the guests of the sending agent
            // on this machine get their own copy.
            guest_gate::after_relayed_gram(&request, &response, api_tx);
            return written;
        }
    }

    // Federation outbound router (W4). BEFORE the local dispatch match — so it
    // covers both the early-match methods (e.g. `agent.prompt`) and the catch-all
    // methods uniformly — a request whose target names a configured peer
    // (`<alias>/…`) is proxied out to that peer and the peer's REAL response is
    // returned verbatim; the local app is never touched. With no configured peer
    // endpoints the registry is empty, this never matches, and behavior is
    // byte-identical to a non-federated daemon.
    if let Some(result) = maybe_route_to_peer(
        &mut stream,
        &mut request,
        federation_peers,
        running,
        &request_id,
        method,
        changes_ui,
    ) {
        return result;
    }

    // Owner-only guest RPCs. Federation peers were refused above (the method
    // table denies every guest.*), and guests never get here.
    if let Some(result) = guest_owner::maybe_handle(
        &mut stream,
        &mut request,
        federation_peers,
        api_tx,
        running,
        changes_ui,
    ) {
        return result;
    }

    match request.method {
        #[cfg(unix)]
        Method::ServerSshAgentRegister(params) => {
            let lease = ssh_agents
                .ok_or_else(|| io::Error::other("SSH agent registration is unavailable"))
                .and_then(|registry| registry.register(PathBuf::from(params.socket_path)));
            let lease = match lease {
                Ok(lease) => lease,
                Err(error) => {
                    return write_text_line_allow_disconnect(
                        &mut stream,
                        &error_response_json(
                            request_id,
                            if error.kind() == io::ErrorKind::InvalidInput {
                                "invalid_ssh_agent"
                            } else {
                                "ssh_agent_unavailable"
                            },
                            error.to_string(),
                        ),
                    )
                }
            };
            write_json_line(
                &mut stream,
                &SuccessResponse {
                    id: request_id,
                    result: ResponseResult::Ok {},
                },
            )?;
            stream.set_polling(true)?;
            let mut byte = [0];
            while running.load(Ordering::Relaxed) {
                match stream.poll_read(&mut byte)? {
                    ApiStreamRead::Pending => {
                        // SSH can unlink an inherited socket after its bridge's lease closes.
                        lease.refresh()?;
                        std::thread::sleep(CONNECTION_POLL_INTERVAL);
                    }
                    _ => break,
                }
            }
            Ok(())
        }
        Method::PaneStream(mut params) => {
            // W5: a `pane.stream` whose target is `<alias>/<remote-pane-id>` names a
            // configured federation peer's agent, so proxy it to that peer and pipe
            // its live-terminal frames back on THIS connection thread — never the
            // single-threaded app loop, and never through the local OutputRing. The
            // outbound W4 router (`maybe_route_to_peer`) skips `pane.stream` on
            // purpose, so this is the only place a federated stream is handled. With
            // no configured peers (or the empty registry the federation-inbound path
            // passes) `federated_stream_target` never matches and the local
            // `pane_output_stream::serve` runs byte-identically to today.
            let result = if let Some((alias, rest, peer_target)) =
                federated_stream_target(&params.pane_id, federation_peers)
            {
                if !peer_target.identity_validated() {
                    let mut stream = stream;
                    write_json_line_allow_disconnect(
                        &mut stream,
                        &federation_identity_unverified_error(request_id.clone(), &alias),
                    )
                } else {
                    // Strip the `<alias>/` prefix so the peer sees its own local pane id;
                    // every other client parameter is forwarded unchanged.
                    params.pane_id = rest;
                    proxy_federated_pane_stream(
                        stream,
                        request_id.clone(),
                        &alias,
                        params,
                        peer_target,
                        running,
                    )
                }
            } else {
                pane_output_stream::serve(stream, request_id.clone(), params, api_tx, running)
            };
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "stream_closed",
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
        Method::PaneInputStream(params) => {
            let result =
                pane_input_stream::serve(stream, request_id.clone(), params, api_tx, running);
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "stream_closed",
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
        Method::GramUploadStream(params) => {
            let result =
                gram_upload_stream::serve(stream, request_id.clone(), params, api_tx, running);
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "stream_closed",
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
        Method::EventsSubscribe(params) => {
            let result = stream_subscriptions(
                stream,
                request_id.clone(),
                params,
                api_tx,
                event_hub,
                running,
            );
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    "stream_closed",
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
        Method::EventsWait(params) => {
            let response = wait_for_event(
                request_id.clone(),
                params,
                &mut stream,
                api_tx,
                event_hub,
                running,
            )?;
            finish_wait_response(&mut stream, response, &request_id, method, changes_ui)
        }
        Method::AgentPrompt(params) => {
            let response = prompt_agent(
                request_id.clone(),
                params,
                &mut stream,
                api_tx,
                event_hub,
                running,
            )?;
            finish_wait_response(&mut stream, response, &request_id, method, changes_ui)
        }
        Method::AgentWait(params) => {
            let response = wait_for_agent(
                request_id.clone(),
                params,
                &mut stream,
                api_tx,
                event_hub,
                running,
            )?;
            finish_wait_response(&mut stream, response, &request_id, method, changes_ui)
        }
        Method::AgentAnswer(params) => {
            let response = answer_agent(request_id.clone(), params, api_tx);
            finish_wait_response(&mut stream, Some(response), &request_id, method, changes_ui)
        }
        Method::PaneWaitForOutput(params) => {
            let response =
                wait_for_output(request_id.clone(), params, &mut stream, api_tx, running)?;
            finish_wait_response(&mut stream, response, &request_id, method, changes_ui)
        }
        method_body => {
            let (response_write_tx, response_write_rx) = std::sync::mpsc::channel();
            // Only the local socket names a process peer; federation and guest
            // connections cannot reach `server.stop` past their gates anyway.
            let stop_caller = match (&method_body, &stream) {
                (Method::ServerStop(_), ApiStream::Local(local)) => {
                    crate::platform::local_stream_peer_description(local)
                }
                _ => None,
            };
            let response = handle_request(
                Request {
                    id: request_id.clone(),
                    method: method_body,
                },
                api_tx,
                capabilities,
                server_stop,
                stop_caller,
                Some(response_write_rx),
            );
            let result = write_text_line_allow_disconnect(&mut stream, &response);
            let _ = response_write_tx.send(());
            match &result {
                Ok(()) => crate::logging::api_request_completed(
                    &request_id,
                    method,
                    api_response_outcome(&response),
                    changes_ui,
                ),
                Err(err) => {
                    crate::logging::api_request_failed(&request_id, method, &err.to_string())
                }
            }
            result
        }
    }
}

fn finish_wait_response(
    stream: &mut ApiStream,
    response: Option<String>,
    request_id: &str,
    method: &'static str,
    changes_ui: bool,
) -> std::io::Result<()> {
    let Some(response) = response else {
        crate::logging::api_request_completed(
            request_id,
            method,
            "client_disconnected",
            changes_ui,
        );
        return Ok(());
    };
    let result = write_text_line_allow_disconnect(stream, &response);
    match &result {
        Ok(()) => crate::logging::api_request_completed(
            request_id,
            method,
            api_response_outcome(&response),
            changes_ui,
        ),
        Err(err) => crate::logging::api_request_failed(request_id, method, &err.to_string()),
    }
    result
}

/// For a method whose target can name a federated remote agent, a mutable
/// reference to that target string; `None` for every other method. The agent
/// methods carry it in `target`; the pane read/turns and pane send-text/send-input
/// methods carry it in `pane_id` (W3 alias-prefixes every identity field, `pane_id`
/// included). This is the exact set the outbound router may proxy — everything else
/// falls through to local dispatch unchanged. `pane.send_text`/`pane.send_input`
/// are the write path the app uses for a federated pane's keystrokes (control keys
/// like S-Tab, `^C`, and Ctrl-chords); both are already `AllowedAt` on the inbound
/// (peer) side, so routing them completes the send symmetry with `agent.send_keys`.
fn routable_target_mut(method: &mut Method) -> Option<&mut String> {
    if !federation_method_policy(api_method_name(method)).outbound_routable {
        return None;
    }
    match method {
        Method::AgentPrompt(params) => Some(&mut params.target),
        Method::AgentSendKeys(params) => Some(&mut params.target),
        Method::AgentGet(params) => Some(&mut params.target),
        Method::AgentRead(params) => Some(&mut params.target),
        Method::AgentTranscript(params) => Some(&mut params.target),
        Method::AgentExplain(params) => Some(&mut params.target),
        // `agent.restart` routes to the owning peer so a federated `<alias>/pane`
        // agent can be restarted from home; the resume runs where the process is.
        Method::AgentRestart(params) => Some(&mut params.target),
        // `agent.answer` drives the selector on the pane's own daemon, which is
        // the one that can read the agent's session file.
        Method::AgentAnswer(params) => Some(&mut params.target),
        // Admin methods that name a pane/agent. `federation.rs` classifies all of these
        // as AllowedAt(Admin) - remote admin is INTENDED - but without an arm here they
        // fall through to `_ => None`, dispatch locally, and fail to resolve an
        // `<alias>/pane` id that only exists on the peer. That produced `agent_not_found`
        // for `agent.rename`/`agent.focus` against a healthy peer whose `agent.read` on
        // the SAME target string worked (#181), because the read methods above are routed
        // and these were not. The tier table and this table must agree; they did not.
        Method::AgentRename(params) => Some(&mut params.target),
        Method::AgentFocus(params) => Some(&mut params.target),
        Method::PaneRename(params) => Some(&mut params.pane_id),
        Method::PaneInputSet(params) => Some(&mut params.pane_id),
        // `pane.send_keys` is Interact tier, beside the already-routed
        // `pane.send_text`; sending text remotely while keys silently went local was
        // the same asymmetry in a different pair.
        Method::PaneSendKeys(params) => Some(&mut params.pane_id),
        // NOT routed, deliberately: `agent.view.set`/`agent.view.clear` are Admin tier
        // but their `source` is a view definition, not a pane or agent id, so there is
        // no peer to resolve it against.
        // Session transfer is deliberately local-only. It reads account-home
        // transcripts and changes harness identity, so an alias-shaped target
        // must not silently gain federation filesystem authority.
        Method::AgentTransferSession(_) => None,
        Method::PaneRead(params) => Some(&mut params.pane_id),
        Method::PaneHistory(params) => Some(&mut params.pane_id),
        Method::PaneTurns(params) => Some(&mut params.pane_id),
        Method::PaneSendText(params) => Some(&mut params.pane_id),
        Method::PaneSendInput(params) => Some(&mut params.pane_id),
        // `pane.set_pty_size` carries the target in `pane_id` too (an
        // `Option<String>`); a `None`/current-pane resize stays local, while a
        // federated `<alias>/pane` routes to the owning peer, where the width
        // arbiter runs (#137).
        Method::PaneSetPtySize(params) => params.pane_id.as_mut(),
        // Workspace/tab reads for a federated `<alias>/…` id run on the owning
        // peer; the reply's ids are re-qualified by `qualify_routed_response`.
        // A `tab.list` without a workspace id stays local.
        Method::WorkspaceGet(params) => Some(&mut params.workspace_id),
        Method::TabGet(params) => Some(&mut params.tab_id),
        Method::TabList(params) => params.workspace_id.as_mut(),
        _ => None,
    }
}

/// Split a target into `(alias, rest)` when it is federated: it must contain a
/// `/` and its first segment must be a configured peer alias. Local ids/names
/// never contain `/` and never equal a configured alias (W3), and a `w1:p1`-style
/// local id has no `/` either — so neither routes. Returns `None` (fall through to
/// local dispatch) for every non-federated target.
fn federated_split<'a, T>(
    target: &'a str,
    peers: &HashMap<String, T>,
) -> Option<(&'a str, &'a str)> {
    let (alias, rest) = target.split_once('/')?;
    if peers.contains_key(alias) {
        Some((alias, rest))
    } else {
        None
    }
}

/// If `request` targets a configured federation peer (`<alias>/…`), proxy it to
/// that peer and return `Some(result)` — the connection is fully handled, the
/// peer's real response already written back. Returns `None` when the request is
/// not federated so the caller dispatches it locally, unchanged.
///
/// On a match the `<alias>/` prefix is stripped from the target before the
/// request is forwarded (the peer knows its agents by their local ids), and the
/// request's own id is preserved so the peer echoes it and the originating client
/// correlates the reply. Owns the completed/failed request logging for the
/// proxied path.
fn maybe_route_to_peer(
    stream: &mut ApiStream,
    request: &mut Request,
    peers: &HashMap<String, PeerRoute>,
    running: &Arc<AtomicBool>,
    request_id: &str,
    method: &'static str,
    changes_ui: bool,
) -> Option<std::io::Result<()>> {
    // Empty registry (the default-off, no-endpoint case) → never route.
    if peers.is_empty() {
        return None;
    }
    let target = routable_target_mut(&mut request.method)?;
    // Decide from owned copies so the mutable borrow is free for the rewrite.
    let (alias, rest) = {
        let (alias, rest) = federated_split(target, peers)?;
        (alias.to_string(), rest.to_string())
    };
    let peer_route = peers.get(&alias)?.clone();
    if !peer_route.identity_validated() {
        let response = federation_identity_unverified_error(request_id.to_owned(), &alias);
        let result = write_json_line_allow_disconnect(stream, &response);
        match &result {
            Ok(()) => crate::logging::api_request_completed(
                request_id,
                method,
                "identity_unverified",
                changes_ui,
            ),
            Err(err) => crate::logging::api_request_failed(request_id, method, &err.to_string()),
        }
        return Some(result);
    }
    let peer_target = peer_route;
    // Federated: strip the `<alias>/` prefix so the peer sees its own local id.
    *target = rest;

    let stamp = peer_target.stamp();
    let response = proxy_federated_response(&peer_target, &stamp, request, running);
    let response = if matches!(method, "workspace.get" | "tab.get" | "tab.list") {
        qualify_routed_response(&alias, &peer_target.presentation(&alias), response)
    } else if method == "agent.transcript" {
        unknown_transcript_as_unsupported(&alias, response)
    } else {
        response
    };
    let result = match peer_target.with_current(&stamp, || {
        write_text_line_allow_disconnect(stream, &response)
    }) {
        Some(result) => result,
        None => {
            let stale = stale_peer_generation_response(request.id.clone(), &stamp);
            write_text_line_allow_disconnect(stream, &stale)
        }
    };
    match &result {
        Ok(()) => crate::logging::api_request_completed(
            request_id,
            method,
            api_response_outcome(&response),
            changes_ui,
        ),
        Err(err) => crate::logging::api_request_failed(request_id, method, &err.to_string()),
    }
    Some(result)
}

/// Forward `request` to the peer reached by `client` and return the response LINE
/// to write back to the originating client.
///
/// Verdict truth: on success the peer's real response is passed through verbatim —
/// the peer's `AgentPrompted { delivery }`, its `Ok {}`, or a `forbidden` it
/// raised from its OWN capability allowlist all reach the caller unchanged; the
/// home applies no capability logic of its own. Failure is classified by phase:
///
/// - a connect/write failure (the request never left the home) → `peer_unreachable`;
/// - a response-read failure AFTER the write (EOF, timeout, oversized, empty) →
///   `delivery_unknown`, because the peer may or may not have acted — and the home
///   NEVER auto-retries, so a write that did land is not duplicated.
///
/// An SSH-bridged peer whose persistent channel is usable carries the request
/// over it (see [`crate::api::peer_channel`]) with the same fence, bounds and
/// classification: a request no byte of which reached the channel is
/// `peer_unreachable`, anything after that is `delivery_unknown`.
fn proxy_federated_response(
    route: &PeerRoute,
    stamp: &PeerRouteStamp,
    request: &Request,
    running: &Arc<AtomicBool>,
) -> String {
    let lease = route
        .peer_channel()
        .and_then(|channel| channel.lease(&request.method, Some(running)));
    let result = if let Some(lease) = lease {
        let Some(sent) = route.with_current(stamp, || lease.send(request)) else {
            return stale_peer_generation_response(request.id.clone(), stamp);
        };
        sent.and_then(|pending| {
            pending.wait(
                &request.id,
                FEDERATION_MAX_RESPONSE_BYTES,
                FEDERATION_PROXY_REQUEST_TIMEOUT,
                Some(running),
            )
        })
    } else {
        let client = ApiClient::for_target(route.target().clone());
        let Some(opened) = route.with_current(stamp, || client.open_proxy_request(request)) else {
            return stale_peer_generation_response(request.id.clone(), stamp);
        };
        opened.and_then(|stream| {
            ApiClient::read_proxy_response_bounded(
                stream,
                FEDERATION_MAX_RESPONSE_BYTES,
                FEDERATION_PROXY_REQUEST_TIMEOUT,
                Some(running),
            )
        })
    };
    match result {
        Ok(line) if route.is_current(stamp) => line,
        Ok(_) => stale_peer_generation_response(request.id.clone(), stamp),
        Err(ProxyError::Connect(err)) => {
            warn!(id = %request.id, err = %err, "federation proxy could not reach peer");
            error_response_json(
                request.id.clone(),
                "peer_unreachable",
                format!("could not reach federation peer: {err}"),
            )
        }
        Err(ProxyError::Read(err)) => {
            warn!(
                id = %request.id,
                err = %err,
                "federation proxy failed after connection; delivery is unknown"
            );
            error_response_json(
                request.id.clone(),
                "delivery_unknown",
                format!(
                    "federation peer may have received the request but completion is unknown: {err}"
                ),
            )
        }
    }
}

fn stale_peer_generation_response(id: String, stamp: &PeerRouteStamp) -> String {
    let boot = stamp.remote_boot_id.as_deref().unwrap_or("unknown");
    error_response_json(
        id,
        "stale_peer_generation",
        format!(
            "federation peer route changed while the request was in flight \
             (generation {}, remote boot {boot})",
            stamp.generation
        ),
    )
}

/// Resolve a `pane.stream` target into `(alias, peer-pane-id, peer connection)`
/// when it names a configured federation peer (`<alias>/<pane-id>`), owning the
/// alias/pane-id so the borrow of `params.pane_id` is released before it is
/// rewritten. Returns `None` — fall through to the LOCAL `pane_output_stream`
/// path — for every non-federated target (no `/`, or an unknown/empty alias). The
/// `peers.get` cannot miss after a [`federated_split`] match, so this never panics.
fn federated_stream_target(
    pane_id: &str,
    peers: &HashMap<String, PeerRoute>,
) -> Option<(String, String, PeerRoute)> {
    if !federation_method_policy("pane.stream").outbound_routable {
        return None;
    }
    let (alias, rest) = federated_split(pane_id, peers)?;
    let target = peers.get(alias)?.clone();
    Some((alias.to_string(), rest.to_string(), target))
}

/// Proxy a federated `pane.stream` to the owning peer and pipe its NDJSON frames
/// back to the requesting client, on the per-connection thread.
///
/// `params.pane_id` is already the peer's LOCAL pane id (the `<alias>/` prefix was
/// stripped by the caller); `alias` is re-applied only to the `stream_started`
/// ack's `pane_id` so the client keeps seeing the federated identity. The client's
/// request id is preserved on the forwarded request so the peer echoes it.
///
/// Phases: (1) connect, then write — connect failure is `peer_unreachable`; any
/// write failure is `delivery_unknown` because bytes may have reached the peer;
/// (2) read the first line, re-prefix and forward a `stream_started` ack, or
/// forward a peer error (`forbidden` from its allowlist, `pane_not_found`)
/// VERBATIM and end, the peer's
/// allowlist staying authoritative with no home-side capability logic; (3) pipe
/// every subsequent frame verbatim until the peer/pane ends, the client drops, or
/// `running` clears. On EVERY exit path the peer stream is dropped, so no orphan
/// stream is left running on the peer.
fn proxy_federated_pane_stream(
    mut client_stream: ApiStream,
    request_id: String,
    alias: &str,
    params: PaneStreamParams,
    peer_route: PeerRoute,
    running: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    let stamp = peer_route.stamp();
    let client = ApiClient::for_target(peer_route.target().clone());
    let request = Request {
        id: request_id.clone(),
        method: Method::PaneStream(params),
    };

    // Phase 1 is lifecycle-linearized through the request write. A retired
    // snapshot cannot connect to or send bytes to its former peer.
    let opened = peer_route.with_current(&stamp, || client.open_frame_stream_classified(&request));
    let Some(opened) = opened else {
        let response = stale_peer_generation_response(request_id, &stamp);
        return write_text_line_allow_disconnect(&mut client_stream, &response);
    };
    let mut peer_stream = match opened {
        Ok(peer_stream) => peer_stream,
        Err(ProxyError::Connect(err)) => {
            warn!(
                id = %request_id,
                alias,
                err = %err,
                "federated pane.stream could not reach peer"
            );
            let response = error_response_json(
                request_id,
                "peer_unreachable",
                format!("could not reach federation peer: {err}"),
            );
            return write_text_line_allow_disconnect(&mut client_stream, &response);
        }
        Err(ProxyError::Read(err)) => {
            warn!(
                id = %request_id,
                alias,
                err = %err,
                "federated pane.stream write failed after connection"
            );
            let response = error_response_json(
                request_id,
                "delivery_unknown",
                format!("federation peer may have received the pane stream request: {err}"),
            );
            return write_text_line_allow_disconnect(&mut client_stream, &response);
        }
    };
    // Phase 2: the first line is the peer's `stream_started` ack or an error line.
    let first = match peer_stream.next_frame(
        FEDERATION_MAX_STREAM_FRAME_BYTES,
        FEDERATION_STREAM_IDLE_TIMEOUT,
        running,
    ) {
        Ok(Some(first)) => first,
        Ok(None) => {
            // Peer closed before answering: it may or may not have opened a stream,
            // so this is delivery-unknown, not a clean not-found.
            let response = error_response_json(
                request_id,
                "delivery_unknown",
                "federation peer closed the pane stream before it started".into(),
            );
            return write_text_line_allow_disconnect(&mut client_stream, &response);
        }
        Err(err) => {
            warn!(
                id = %request_id,
                alias,
                err = %err,
                "federated pane.stream: reading the peer stream_started failed"
            );
            let response = error_response_json(
                request_id,
                "delivery_unknown",
                format!("federation peer pane stream could not be read: {err}"),
            );
            return write_text_line_allow_disconnect(&mut client_stream, &response);
        }
    };

    // Re-prefix a `stream_started` ack's pane_id back to `<alias>/…`; forward a peer
    // error verbatim. `keep_streaming` is false for anything but a real ack, so a
    // peer error ends the proxy after this single line.
    let (first_line, keep_streaming) = prepare_first_stream_line(&first, alias);
    match peer_route.with_current(&stamp, || {
        emit_line_to_client(&mut client_stream, &first_line)
    }) {
        Some(result) => {
            if !result? || !keep_streaming {
                return Ok(());
            }
        }
        None => {
            let response = stale_peer_generation_response(request_id, &stamp);
            return write_text_line_allow_disconnect(&mut client_stream, &response);
        }
    }

    // Phase 3: pipe every subsequent frame verbatim. Read-one / write-one: a slow
    // client blocks the write, which blocks the read, so the peer's OutputRing (and
    // its snapshot-collapse) absorbs backpressure — the home buffers nothing.
    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        if !peer_route.is_current(&stamp) {
            let response = stale_peer_generation_response(request_id.clone(), &stamp);
            let _ = write_text_line_allow_disconnect(&mut client_stream, &response);
            break;
        }
        match peer_stream.next_frame(
            FEDERATION_MAX_STREAM_FRAME_BYTES,
            FEDERATION_STREAM_IDLE_TIMEOUT,
            running,
        ) {
            Ok(Some(frame)) => {
                match peer_route
                    .with_current(&stamp, || emit_line_to_client(&mut client_stream, &frame))
                {
                    Some(Ok(true)) => {}
                    Some(Ok(false)) => break,
                    Some(Err(error)) => return Err(error),
                    None => {
                        let response = stale_peer_generation_response(request_id.clone(), &stamp);
                        let _ = write_text_line_allow_disconnect(&mut client_stream, &response);
                        break;
                    }
                }
            }
            Ok(None) => break, // peer/pane ended (sent `exited` then closed)
            Err(err) => {
                // Idle timeout, over-cap frame, transport error, or shutdown: close
                // the proxied stream. Shutdown (`Interrupted`) is expected and quiet.
                if err.kind() != std::io::ErrorKind::Interrupted {
                    warn!(
                        id = %request_id,
                        alias,
                        err = %err,
                        "federated pane.stream read ended"
                    );
                }
                break;
            }
        }
    }

    // Every exit path reaches here: dropping the peer stream closes the peer
    // connection so no orphan stream is left running on the peer.
    drop(peer_stream);
    Ok(())
}

/// Prepare the first line of a proxied `pane.stream` for the client, returning
/// `(line, keep_streaming)`.
///
/// A `stream_started` success has its `pane_id` re-prefixed to `<alias>/…` (the
/// federated identity the client expects) and is re-serialized; `keep_streaming`
/// is true so the frame-piping loop runs. Anything else — an error line the peer
/// raised (`forbidden`, `pane_not_found`), or an unexpected/unparseable line — is
/// forwarded VERBATIM with `keep_streaming` false, ending the proxy after it. The
/// home applies NO capability logic of its own; the peer's allowlist is
/// authoritative.
fn prepare_first_stream_line(first: &str, alias: &str) -> (String, bool) {
    match serde_json::from_str::<SuccessResponse>(first) {
        Ok(mut success) => {
            if let ResponseResult::StreamStarted { pane_id, .. } = &mut success.result {
                let reprefixed = format!("{alias}/{pane_id}");
                *pane_id = reprefixed;
                match serde_json::to_string(&success) {
                    Ok(line) => (line, true),
                    // Re-encoding a value we just decoded should not fail; if it
                    // somehow does, forward the peer's original line unchanged.
                    Err(_) => (first.to_string(), true),
                }
            } else {
                // A non-`stream_started` success is not expected as a first line;
                // forward it verbatim and stop.
                (first.to_string(), false)
            }
        }
        // An error line (e.g. `forbidden`, `pane_not_found`) or non-JSON: verbatim.
        Err(_) => (first.to_string(), false),
    }
}

/// Write one raw NDJSON line to the requesting client, returning `false` when the
/// client has closed so the proxy loop can stop cleanly. Mirrors the `emit`
/// pattern in [`pane_output_stream`]: a genuine (non-disconnect) write error still
/// propagates. A wedged socket write is bounded by the connection-wide send
/// timeout and tears down only this connection.
fn emit_line_to_client(stream: &mut ApiStream, line: &str) -> std::io::Result<bool> {
    match write_text_line(stream, line) {
        Ok(()) => Ok(true),
        Err(err) if is_connection_closed_error(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
pub(crate) fn handle_reverse_gram(request: Request, tx: &ApiRequestSender) -> String {
    // Only api::reverse creates this envelope, after accepting its seven-method
    // whitelist on the per-peer SSH gateway. The regular TCP policy still denies
    // gram.* and the local API remains local.
    handle_request(request, tx, None, None, None, None)
}

fn handle_request(
    request: Request,
    api_tx: &ApiRequestSender,
    capabilities: Option<ServerCapabilities>,
    server_stop: Option<&ServerStop>,
    stop_caller: Option<String>,
    response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
) -> String {
    if matches!(&request.method, Method::Ping(_)) {
        return serde_json::to_string(&SuccessResponse {
            id: request.id,
            result: ResponseResult::Pong {
                version: crate::build_info::version(),
                protocol: crate::protocol::PROTOCOL_VERSION,
                capabilities,
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
                .to_string()
        });
    }

    if matches!(&request.method, Method::ClientShellSurfaceSet(_)) {
        return error_response_json(
            request.id,
            "connection_local_only",
            "client_shell.surface.set is only available through a client shell endpoint".into(),
        );
    }

    if matches!(&request.method, Method::ServerStop(_)) {
        if let Some(server_stop) = server_stop {
            server_stop.request(ShutdownReason::ApiStop {
                caller: stop_caller,
            });
            return serde_json::to_string(&SuccessResponse {
                id: request.id,
                result: ResponseResult::Ok {},
            })
            .unwrap_or_else(|_| "{}".to_string());
        }
    } else if server_stop.is_some_and(ServerStop::is_requested) {
        return error_response_json(
            request.id,
            "server_unavailable",
            "server is shutting down".into(),
        );
    }

    dispatch_to_app(request, api_tx, None, response_write_complete, None)
}

pub(crate) fn api_method_name(method: &Method) -> &'static str {
    method.operation().name
}

fn api_response_outcome(response: &str) -> &'static str {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(response) else {
        return "error";
    };

    match value
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_str())
    {
        Some("timeout") => "timeout",
        Some(_) => "error",
        None => "ok",
    }
}

fn read_initial_request_line(stream: &mut ApiStream) -> std::io::Result<Option<String>> {
    read_initial_request_line_with_timeout(stream, INITIAL_REQUEST_TIMEOUT)
}

fn read_initial_request_line_with_timeout(
    stream: &mut ApiStream,
    timeout: Duration,
) -> std::io::Result<Option<String>> {
    read_initial_request_line_with_limits(stream, timeout, MAX_INITIAL_REQUEST_BYTES)
}

fn read_initial_request_line_with_limits(
    stream: &mut ApiStream,
    timeout: Duration,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    stream.set_polling(true)?;
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];

    let result = loop {
        let read = match stream.poll_read(&mut byte) {
            Ok(read) => read,
            Err(err) => break Err(err),
        };
        match read {
            ApiStreamRead::Closed => break Ok(None),
            ApiStreamRead::Data(_) => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break String::from_utf8(bytes)
                        .map(Some)
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
                }
                if bytes.len() > max_bytes {
                    break Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "api request line is too large",
                    ));
                }
            }
            ApiStreamRead::Pending => {
                if Instant::now() >= deadline {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out reading api request",
                    ));
                }
                std::thread::sleep(CONNECTION_POLL_INTERVAL);
            }
        }
    };
    stream.set_polling(false)?;
    result
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc::{self, Receiver};

    fn local_stream_pair(name: &str) -> (LocalStream, LocalStream, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "herdr-api-{name}-{}-{}.sock",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, path)
    }

    fn spawn_connection(
        server: LocalStream,
    ) -> (Receiver<std::io::Result<()>>, std::thread::JoinHandle<()>) {
        let (done_tx, done_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let (api_tx, _api_rx) = tokio::sync::mpsc::unbounded_channel();
            let result = handle_connection(
                server,
                &api_tx,
                &EventHub::default(),
                &Arc::new(AtomicBool::new(true)),
                None,
            );
            done_tx.send(result).unwrap();
        });
        (done_rx, thread)
    }

    #[test]
    fn windows_delayed_partial_initial_request_returns_pong() {
        let (mut client, server, path) = local_stream_pair("delayed-request");
        let (done_rx, server_thread) = spawn_connection(server);

        std::thread::sleep(Duration::from_millis(300));
        assert!(
            done_rx.try_recv().is_err(),
            "idle connected client must not be treated as closed"
        );

        client
            .write_all(br#"{"id":"delayed","method":"ping","params":{}}"#)
            .unwrap();
        client.flush().unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            done_rx.try_recv().is_err(),
            "partial request must wait for its newline"
        );
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let mut response = String::new();
        BufReader::new(&mut client)
            .read_line(&mut response)
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], "delayed");
        assert_eq!(response["result"]["type"], "pong");

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        server_thread.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn windows_disconnected_initial_request_returns_promptly() {
        let (client, server, path) = local_stream_pair("disconnected-request");
        let (done_rx, server_thread) = spawn_connection(server);

        drop(client);

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("disconnected connection handler must finish promptly")
            .unwrap();
        server_thread.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn windows_idle_initial_request_honors_timeout() {
        let (_client, server, path) = local_stream_pair("request-timeout");
        let mut server = ApiStream::Local(server);

        let err = read_initial_request_line_with_timeout(&mut server, Duration::from_millis(50))
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn windows_initial_request_enforces_size_limit() {
        let (mut client, server, path) = local_stream_pair("request-size-limit");
        client.write_all(b"12345").unwrap();
        client.flush().unwrap();
        let mut server = ApiStream::Local(server);

        let err = read_initial_request_line_with_limits(&mut server, Duration::from_secs(1), 4)
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "api request line is too large");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn windows_initial_request_rejects_invalid_utf8() {
        let (mut client, server, path) = local_stream_pair("request-invalid-utf8");
        client.write_all(&[0xff, b'\n']).unwrap();
        client.flush().unwrap();
        let mut server = ApiStream::Local(server);

        let err = read_initial_request_line_with_timeout(&mut server, Duration::from_secs(1))
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(path);
    }
}

/// How long an `events_v2` subscription stream may stay silent before it
/// writes a heartbeat line. Well under [`FEDERATION_STREAM_IDLE_TIMEOUT`], so a
/// peer relaying the stream never closes a quiet but healthy subscription.
const SUBSCRIPTION_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

fn stream_subscriptions(
    stream: ApiStream,
    request_id: String,
    params: crate::api::schema::EventsSubscribeParams,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    stream_subscriptions_with_heartbeat(
        stream,
        request_id,
        params,
        api_tx,
        event_hub,
        running,
        SUBSCRIPTION_HEARTBEAT_INTERVAL,
    )
}

fn stream_subscriptions_with_heartbeat(
    mut stream: ApiStream,
    request_id: String,
    params: crate::api::schema::EventsSubscribeParams,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
    heartbeat_interval: Duration,
) -> std::io::Result<()> {
    let events_v2 = params.events_v2;
    let local_only = params.local_only;
    let event_start_sequence = event_hub.current_sequence();
    let mut subscriptions = Vec::with_capacity(params.subscriptions.len());
    let mut rejected = Vec::new();
    for (index, subscription) in params.subscriptions.into_iter().enumerate() {
        match ActiveSubscription::new(
            subscription,
            &request_id,
            index,
            api_tx,
            event_hub,
            event_start_sequence,
        ) {
            Ok(active) => subscriptions.push(active),
            // A pane can close between listing and subscribing; v2 clients get
            // the rest of their entries instead of a failed request.
            Err(response) if events_v2 && response.error.code == "pane_not_found" => {
                rejected.push(crate::api::schema::SubscriptionRejection {
                    index,
                    error: response.error,
                });
            }
            Err(mut response) => {
                response.id = request_id;
                return write_json_line_allow_disconnect(&mut stream, &response);
            }
        }
    }

    if !write_subscription_line(
        &mut stream,
        &SuccessResponse {
            id: request_id.clone(),
            result: ResponseResult::SubscriptionStarted { rejected },
        },
    )? {
        return Ok(());
    }

    let mut cursor = event_start_sequence;
    let mut last_write = Instant::now();
    let mut lines = Vec::new();
    loop {
        if should_stop_connection(&mut stream, running)? {
            return Ok(());
        }

        let batch = match event_hub.read_checked(cursor, !local_only) {
            Ok(batch) => batch,
            Err(error) => {
                return write_json_line_allow_disconnect(
                    &mut stream,
                    &ErrorResponse {
                        id: request_id,
                        error: history_error(error),
                    },
                );
            }
        };
        // A legacy reader that fell behind retained history gets `events_lost`
        // and the stream closes before it sees a partial stream; an `events_v2`
        // reader gets a `lagged` line below and continues.
        if batch.missed.is_some() && !events_v2 {
            return write_json_line_allow_disconnect(
                &mut stream,
                &ErrorResponse {
                    id: request_id,
                    error: history_error(EventHistoryError::Lost),
                },
            );
        }
        cursor = batch.head;
        for (index, subscription) in subscriptions.iter_mut().enumerate() {
            lines.extend(
                subscription
                    .poll(api_tx, event_hub, &batch)
                    .into_iter()
                    .map(|line| (index, line)),
            );
        }
        // One hub event can match several entries; keep hub order across
        // entries and request order among lines that share a sequence.
        lines.sort_by_key(|(index, line)| (line.seq, line.derived, *index));

        let mut wrote = false;
        if let Some(missed) = batch.missed {
            let lagged = crate::api::schema::SubscriptionControlLine::Lagged {
                seq: missed.last,
                first_missed_seq: missed.first,
                last_missed_seq: missed.last,
            };
            if !write_subscription_line(&mut stream, &lagged)? {
                return Ok(());
            }
            wrote = true;
        }
        for (_, line) in lines.drain(..) {
            let event = crate::api::schema::SubscriptionStreamEvent {
                seq: line.seq,
                payload: line.payload,
            };
            if !write_subscription_line(&mut stream, &event)? {
                return Ok(());
            }
            wrote = true;
        }
        if wrote {
            last_write = Instant::now();
        } else if events_v2 && last_write.elapsed() >= heartbeat_interval {
            let heartbeat = crate::api::schema::SubscriptionControlLine::Heartbeat { seq: cursor };
            if !write_subscription_line(&mut stream, &heartbeat)? {
                return Ok(());
            }
            last_write = Instant::now();
        }
        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

/// Writes one subscription stream line; `Ok(false)` means the client left.
fn write_subscription_line<T: serde::Serialize>(
    stream: &mut ApiStream,
    value: &T,
) -> std::io::Result<bool> {
    match write_json_line(stream, value) {
        Ok(()) => Ok(true),
        Err(err) if is_connection_closed_error(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

fn write_text_line(stream: &mut ApiStream, value: &str) -> std::io::Result<()> {
    stream.write_all(value.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn write_text_line_allow_disconnect(stream: &mut ApiStream, value: &str) -> std::io::Result<()> {
    match write_text_line(stream, value) {
        Err(err) if is_connection_closed_error(&err) => Ok(()),
        result => result,
    }
}

fn write_json_line<T: serde::Serialize>(stream: &mut ApiStream, value: &T) -> std::io::Result<()> {
    let encoded = serde_json::to_string(value)
        .map_err(|err| std::io::Error::other(format!("failed to encode json: {err}")))?;
    write_text_line(stream, &encoded)
}

fn write_json_line_allow_disconnect<T: serde::Serialize>(
    stream: &mut ApiStream,
    value: &T,
) -> std::io::Result<()> {
    let encoded = serde_json::to_string(value)
        .map_err(|err| std::io::Error::other(format!("failed to encode json: {err}")))?;
    write_text_line_allow_disconnect(stream, &encoded)
}

pub(super) fn should_stop_connection(
    stream: &mut ApiStream,
    running: &Arc<AtomicBool>,
) -> std::io::Result<bool> {
    if !running.load(Ordering::Relaxed) {
        return Ok(true);
    }

    stream.peer_closed()
}

pub(super) fn dispatch_to_app_with_timeout(
    request: Request,
    api_tx: &ApiRequestSender,
    timeout: Option<Duration>,
) -> String {
    dispatch_to_app(request, api_tx, timeout, None, None)
}

pub(super) fn dispatch_to_app_with_caller_timeout(
    request: Request,
    api_tx: &ApiRequestSender,
    timeout: Option<Duration>,
) -> String {
    dispatch_to_app(
        request,
        api_tx,
        timeout,
        None,
        Some(("timeout", "timed out waiting for agent status")),
    )
}

fn dispatch_to_app(
    request: Request,
    api_tx: &ApiRequestSender,
    timeout: Option<Duration>,
    response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
    timeout_response: Option<(&str, &str)>,
) -> String {
    let request_id = request.id.clone();
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    if let Err(err) = api_tx.send(ApiRequestMessage {
        request,
        respond_to,
        response_write_complete,
    }) {
        return error_response_json(
            request_id,
            "server_unavailable",
            format!("failed to dispatch request: {err}"),
        );
    }

    let response = match timeout {
        Some(timeout) => response_rx.recv_timeout(timeout).map_err(|err| match err {
            std::sync::mpsc::RecvTimeoutError::Timeout => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "timed out waiting for app response after {} ms",
                    timeout.as_millis()
                ),
            ),
            std::sync::mpsc::RecvTimeoutError::Disconnected => std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "app response channel closed",
            ),
        }),
        None => response_rx
            .recv()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::BrokenPipe, err)),
    };

    match response {
        Ok(response) => response,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::TimedOut {
                if let Some((code, message)) = timeout_response {
                    return error_response_json(request_id, code, message.into());
                }
            }
            error_response_json(
                request_id,
                "server_unavailable",
                format!("request handling failed: {err}"),
            )
        }
    }
}

#[cfg(test)]
#[test]
fn caller_timeout_dispatch_uses_timeout_error() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let response = dispatch_to_app_with_caller_timeout(
        Request {
            id: "prompt-timeout".into(),
            method: Method::AgentPrompt(crate::api::schema::AgentPromptParams {
                target: "reviewer".into(),
                text: "review this".into(),
                wait: None,
            }),
        },
        &tx,
        Some(Duration::ZERO),
    );
    let error: ErrorResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(error.error.code, "timeout");
}

fn error_response_json(id: String, code: &str, message: String) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: code.into(),
            message,
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode error response"}}"#
            .to_string()
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use interprocess::local_socket::traits::Listener as _;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::sync::{Mutex, OnceLock};
    use tokio::sync::mpsc;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_test_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
    }

    fn read_line(stream: &mut LocalStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    fn local_stream_pair(name: &str) -> (LocalStream, LocalStream, PathBuf) {
        let path = unique_test_path(name);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, path)
    }

    #[test]
    fn ssh_agent_registration_lasts_only_for_the_api_connection() {
        let directory = unique_test_path("agent-lease");
        fs::create_dir(&directory).unwrap();
        let agent = directory.join("upstream");
        let _agent = UnixListener::bind(&agent).unwrap();
        let stable = directory.join("stable");
        let registry =
            crate::platform::ssh_agent::SshAgentRegistry::new(stable.clone(), None).unwrap();
        let (mut client, server, api_path) = local_stream_pair("agent-api");
        let (tx, _rx) = mpsc::unbounded_channel();
        let worker_registry = registry.clone();
        let worker = std::thread::spawn(move || {
            handle_connection_with_stop(
                ApiStream::Local(server),
                &tx,
                &EventHub::default(),
                &Arc::new(AtomicBool::new(true)),
                None,
                None,
                None,
                &HashMap::new(),
                Some(&worker_registry),
            )
            .unwrap();
        });
        let request = serde_json::to_string(&Request {
            id: "agent-lease".into(),
            method: Method::ServerSshAgentRegister(
                crate::api::schema::ServerSshAgentRegisterParams {
                    socket_path: agent.to_string_lossy().into_owned(),
                },
            ),
        })
        .unwrap();
        std::io::Write::write_all(&mut client, format!("{request}\n").as_bytes()).unwrap();
        let response: SuccessResponse = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert!(matches!(response.result, ResponseResult::Ok {}));
        assert_eq!(fs::read_link(&stable).unwrap(), agent);
        drop(client);
        worker.join().unwrap();
        assert!(!stable.exists());
        drop(registry);
        fs::remove_file(api_path).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    fn pane_info(
        pane_id: &str,
        agent_status: crate::api::schema::AgentStatus,
    ) -> crate::api::schema::PaneInfo {
        crate::api::schema::PaneInfo {
            pane_id: pane_id.into(),
            terminal_id: "term_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            focused: true,
            cwd: None,
            foreground_cwd: None,
            restore_error: None,
            label: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status,
            input_pending: false,
            input_prompt_kind: None,
            composer: Default::default(),
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            last_completed_turn: None,
            turn: Some(0),
            turn_epoch: Some(9),
            scroll: None,
            alternate_screen: false,
            revision: 0,
        }
    }

    fn spawn_pane_get_responder(
        agent_status: crate::api::schema::AgentStatus,
    ) -> (ApiRequestSender, std::thread::JoinHandle<()>) {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let responder = std::thread::spawn(move || {
            while let Some(msg) = api_rx.blocking_recv() {
                match msg.request.method {
                    Method::PaneGet(_) => msg
                        .respond_to
                        .send(
                            serde_json::to_string(&SuccessResponse {
                                id: msg.request.id,
                                result: ResponseResult::PaneInfo {
                                    pane: pane_info("pane_1", agent_status),
                                },
                            })
                            .unwrap(),
                        )
                        .unwrap(),
                    Method::EventsWait(_) => msg
                        .respond_to
                        .send(error_response_json(
                            msg.request.id,
                            "unexpected_dispatch",
                            "events.wait should be handled by the api server".into(),
                        ))
                        .unwrap(),
                    other => panic!("unexpected request: {other:?}"),
                }
            }
        });
        (api_tx, responder)
    }

    #[test]
    fn api_accept_thread_spawn_failure_removes_bound_socket() {
        let _guard = env_lock().lock().unwrap();
        let dir = unique_test_path("api-accept-spawn-failure");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("herdr.sock");
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, &path);
        let (api_tx, _api_rx) = mpsc::unbounded_channel();

        crate::thread_spawn::test_hook::fail_next_spawns(1);
        let result = start_server_inner(
            api_tx,
            EventHub::default(),
            None,
            None,
            &FederationConfig::default(),
            Arc::new(Mutex::new(FederationStore::default())),
        );
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);

        assert!(result.is_err());
        assert!(!path.exists(), "bound socket must be removed");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn socket_path_prefers_explicit_env_override() {
        let _guard = env_lock().lock().unwrap();
        let unique = format!("/tmp/herdr-test-{}.sock", std::process::id());
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, &unique);
        assert_eq!(socket_path(), PathBuf::from(&unique));
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn socket_path_defaults_to_config_dir_even_when_xdg_runtime_dir_is_set() {
        let _guard = env_lock().lock().unwrap();
        let config_home = unique_test_path("socket-default-config-home");
        let runtime_dir = unique_test_path("socket-default-runtime");
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);

        let expected = config_home
            .join(crate::config::app_dir_name())
            .join("herdr.sock");
        assert_eq!(socket_path(), expected);

        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn socket_path_uses_named_session_dir() {
        let _guard = env_lock().lock().unwrap();
        let config_home = unique_test_path("socket-named-config-home");
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        std::env::set_var(crate::session::SESSION_ENV_VAR, "work");
        std::env::set_var("XDG_CONFIG_HOME", &config_home);

        let expected = config_home
            .join(crate::config::app_dir_name())
            .join("sessions")
            .join("work")
            .join("herdr.sock");
        assert_eq!(socket_path(), expected);

        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn federation_listener_binds_only_for_sessions_that_federate() {
        let dir = unique_test_path("federation-listener-session");
        fs::create_dir_all(&dir).unwrap();
        let token_file = dir.join("laptop.token");
        fs::write(&token_file, "laptop-token\n").unwrap();
        let mut federation = FederationConfig {
            listen: true,
            listen_addr: Some("127.0.0.1:0".into()),
            peers: vec![FederationPeer {
                alias: "laptop".into(),
                token_file: Some(token_file.to_string_lossy().into_owned()),
                ..FederationPeer::default()
            }],
            ..FederationConfig::default()
        };
        let (api_tx, _api_rx) = mpsc::unbounded_channel();
        let event_hub = EventHub::default();
        let running = Arc::new(AtomicBool::new(true));
        let start = |federation: &FederationConfig, session| {
            maybe_start_federation_listener(
                federation, session, &api_tx, &event_hub, &None, &running, &None,
            )
        };

        assert!(
            start(&federation, Some("work")).is_none(),
            "an unlisted named session must leave listen_addr to the default session"
        );
        let default_session = start(&federation, None).expect("default session binds");
        federation.named_sessions = vec!["work".into()];
        let listed_session = start(&federation, Some("work")).expect("listed session binds");

        running.store(false, Ordering::Relaxed);
        default_session.join().unwrap();
        listed_session.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restrict_socket_permissions_sets_user_only_mode() {
        let dir = unique_test_path("socket-perms");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("api.sock");
        let _listener = UnixListener::bind(&path).unwrap();

        restrict_socket_permissions(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_PERMISSION_MODE);

        drop(_listener);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_response_outcome_uses_top_level_error_shape() {
        let ok_with_error_text = r#"{"id":"req","result":{"read":{"text":"user said \"error\": \"timeout\"","revision":1}}}"#;
        assert_eq!(api_response_outcome(ok_with_error_text), "ok");

        let timeout = r#"{"id":"req","error":{"code":"timeout","message":"timed out waiting for output match"}}"#;
        assert_eq!(api_response_outcome(timeout), "timeout");

        let generic_error =
            r#"{"id":"req","error":{"code":"server_unavailable","message":"boom"}}"#;
        assert_eq!(api_response_outcome(generic_error), "error");
    }

    #[test]
    fn removed_pane_graphics_methods_return_unknown_method_without_stream_upgrade() {
        for method in [
            "pane.graphics.info",
            "pane.graphics.set",
            "pane.graphics.clear",
            "pane.graphics.stream",
        ] {
            let (mut client, server, _path) = local_stream_pair("removed-pane-graphics");
            let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
            writeln!(
                client,
                "{{\"id\":\"removed\",\"method\":\"{method}\",\"params\":{{}}}}"
            )
            .unwrap();
            client.flush().unwrap();

            handle_connection(
                server,
                &api_tx,
                &EventHub::default(),
                &Arc::new(AtomicBool::new(true)),
                None,
            )
            .unwrap();

            let response = read_line(&mut client);
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["id"], "removed", "{method}");
            assert_eq!(response["error"]["code"], "unknown_method", "{method}");
            assert!(response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(method)));
            assert!(response.get("result").is_none(), "{method}");
            assert!(api_rx.try_recv().is_err(), "{method} reached the app");
        }
    }

    #[test]
    fn unrelated_unknown_method_retains_standard_invalid_request_response() {
        let (mut client, server, _path) = local_stream_pair("unknown-api-request");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        client
            .write_all(b"{\"id\":\"unknown\",\"method\":\"nope\",\"params\":{}}\n")
            .unwrap();
        client.flush().unwrap();

        handle_connection(
            server,
            &api_tx,
            &EventHub::default(),
            &Arc::new(AtomicBool::new(true)),
            None,
        )
        .unwrap();

        let response = read_line(&mut client);
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], "unknown");
        assert_eq!(response["error"]["code"], "invalid_request");
        assert!(api_rx.try_recv().is_err());
    }

    #[test]
    fn ordinary_api_request_still_uses_normal_connection_path() {
        let (mut client, server, _path) = local_stream_pair("ordinary-api-request");
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        client
            .write_all(b"{\"id\":\"ordinary\",\"method\":\"ping\",\"params\":{}}\n")
            .unwrap();
        client.flush().unwrap();

        handle_connection(
            server,
            &api_tx,
            &EventHub::default(),
            &Arc::new(AtomicBool::new(true)),
            None,
        )
        .unwrap();

        let response = read_line(&mut client);
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], "ordinary");
        assert_eq!(response["result"]["type"], "pong");
    }

    #[test]
    fn ping_request_returns_pong() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let response = handle_request(
            Request {
                id: "req_1".into(),
                method: Method::Ping(crate::api::schema::PingParams::default()),
            },
            &tx,
            Some(ServerCapabilities {
                live_handoff: true,
                detached_server_daemon: true,
                endpoint_protocol_generation: Some(
                    crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
                ),
                surface_interest: true,
                health_check: true,
                pane_input_stream: false,
                gram_upload_stream: false,
                agent_session_transfer: true,
                agent_session_transfer_harnesses: vec![
                    crate::api::schema::AgentSessionTransferHarness::Claude,
                    crate::api::schema::AgentSessionTransferHarness::Codex,
                    crate::api::schema::AgentSessionTransferHarness::Omp,
                ],
                events_v2: false,
                ssh_agent_registration: false,
                agent_forget: false,
                api_bridge_multi: false,
                pane_history: false,
                agent_answer: false,
                agent_transcript: false,
            }),
            None,
            None,
            None,
        );

        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_1");
        assert!(matches!(parsed.result, ResponseResult::Pong { .. }));
    }

    #[test]
    fn server_stop_control_bypasses_app_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let stop = ServerStop::default();
        let response = handle_request(
            Request {
                id: "priority_stop".into(),
                method: Method::ServerStop(crate::api::schema::EmptyParams::default()),
            },
            &tx,
            None,
            Some(&stop),
            Some("pid 42 (herdr)".into()),
            None,
        );

        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], "priority_stop");
        assert_eq!(response["result"]["type"], "ok");
        assert!(stop.is_requested());
        assert_eq!(
            stop.take_reason(),
            Some(ShutdownReason::ApiStop {
                caller: Some("pid 42 (herdr)".into())
            })
        );

        let rejected = handle_request(
            Request {
                id: "after_stop".into(),
                method: Method::WorkspaceList(crate::api::schema::WorkspaceListParams::default()),
            },
            &tx,
            None,
            Some(&stop),
            None,
            None,
        );
        let rejected: serde_json::Value = serde_json::from_str(&rejected).unwrap();
        assert_eq!(rejected["error"]["code"], "server_unavailable");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn request_dispatches_to_app_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let request = Request {
            id: "req_2".into(),
            method: Method::WorkspaceList(crate::api::schema::WorkspaceListParams::default()),
        };

        let request_for_thread = request.clone();
        let thread = std::thread::spawn(move || {
            handle_request(request_for_thread, &tx, None, None, None, None)
        });

        let msg = rx.blocking_recv().unwrap();
        assert_eq!(msg.request.id, "req_2");
        msg.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: "req_2".into(),
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let response = thread.join().unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_2");
    }

    #[test]
    fn dispatched_request_reports_response_write_completion() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel();
        let (mut client, server, _path) = local_stream_pair("write-ack");
        client
            .write_all(br#"{"id":"req_write","method":"workspace.list","params":{}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        let msg = api_rx.blocking_recv().unwrap();
        let response_write_complete = msg
            .response_write_complete
            .expect("socket-dispatched requests include write completion");
        msg.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: msg.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        response_write_complete
            .recv_timeout(Duration::from_secs(1))
            .expect("response write completion");
        let response: SuccessResponse = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert_eq!(response.id, "req_write");
        server_thread.join().unwrap().unwrap();
    }

    #[test]
    fn events_wait_agent_status_returns_initial_match() {
        let (api_tx, responder) =
            spawn_pane_get_responder(crate::api::schema::AgentStatus::Blocked);

        let (mut client, server, _path) = local_stream_pair("api-events-wait-initial");
        client
            .write_all(br#"{"id":"wait_1","method":"events.wait","params":{"match_event":{"event":"pane_agent_status_changed","pane_id":"pane_1","agent_status":"blocked"},"timeout_ms":1000}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let event_hub = EventHub::default();
        handle_connection(server, &api_tx, &event_hub, &running, None).unwrap();

        let response: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert_eq!(response["id"], "wait_1");
        assert_eq!(response["result"]["type"], "wait_matched");
        assert_eq!(
            response["result"]["event"]["data"]["agent_status"],
            "blocked"
        );
        assert_eq!(response["result"]["event"]["data"]["turn"], 0);
        assert_eq!(response["result"]["event"]["data"]["turn_epoch"], 9);
        drop(api_tx);
        responder.join().unwrap();
    }

    #[test]
    fn events_wait_agent_status_times_out_server_side() {
        let (api_tx, responder) =
            spawn_pane_get_responder(crate::api::schema::AgentStatus::Unknown);

        let (mut client, server, _path) = local_stream_pair("api-events-wait-timeout");
        client
            .write_all(br#"{"id":"wait_2","method":"events.wait","params":{"match_event":{"event":"pane_agent_status_changed","pane_id":"pane_1","agent_status":"blocked"},"timeout_ms":30}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let event_hub = EventHub::default();
        handle_connection(server, &api_tx, &event_hub, &running, None).unwrap();

        let response: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert_eq!(response["id"], "wait_2");
        assert_eq!(response["error"]["code"], "timeout");
        assert_eq!(
            response["error"]["message"],
            "timed out waiting for event match"
        );
        drop(api_tx);
        responder.join().unwrap();
    }

    #[test]
    fn events_wait_agent_status_returns_not_found_when_pane_closes() {
        let event_hub = EventHub::default();
        let responder_event_hub = event_hub.clone();
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let responder = std::thread::spawn(move || {
            let mut pane_get_count = 0;
            while let Some(msg) = api_rx.blocking_recv() {
                let Method::PaneGet(_) = msg.request.method else {
                    panic!("unexpected request: {:?}", msg.request.method);
                };
                pane_get_count += 1;
                let response = if pane_get_count == 1 {
                    serde_json::to_string(&SuccessResponse {
                        id: msg.request.id,
                        result: ResponseResult::PaneInfo {
                            pane: pane_info("pane_1", crate::api::schema::AgentStatus::Unknown),
                        },
                    })
                    .unwrap()
                } else {
                    if pane_get_count == 2 {
                        responder_event_hub.push(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::PaneClosed,
                            data: crate::api::schema::EventData::PaneClosed {
                                pane_id: "pane_1".into(),
                                workspace_id: "ws_1".into(),
                            },
                        });
                    }
                    error_response_json(
                        msg.request.id,
                        "pane_not_found",
                        "pane pane_1 not found".into(),
                    )
                };
                msg.respond_to.send(response).unwrap();
            }
        });

        let (mut client, server, _path) = local_stream_pair("wait-close");
        client
            .write_all(br#"{"id":"wait_close","method":"events.wait","params":{"match_event":{"event":"pane_agent_status_changed","pane_id":"pane_1","agent_status":"done"},"timeout_ms":500}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        handle_connection(server, &api_tx, &event_hub, &running, None).unwrap();

        let response: serde_json::Value = serde_json::from_str(&read_line(&mut client)).unwrap();
        assert_eq!(response["id"], "wait_close");
        assert_eq!(response["error"]["code"], "pane_not_found");
        assert_eq!(response["error"]["message"], "pane pane_1 not found");
        drop(api_tx);
        responder.join().unwrap();
    }

    #[test]
    fn wait_for_output_stops_when_client_disconnects() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (first_read_tx, first_read_rx) = std::sync::mpsc::channel();
        let responder = std::thread::spawn(move || {
            let mut notified = false;
            while let Some(msg) = api_rx.blocking_recv() {
                assert!(matches!(msg.request.method, Method::PaneRead(_)));
                if !notified {
                    first_read_tx.send(()).unwrap();
                    notified = true;
                }
                msg.respond_to
                    .send(
                        serde_json::to_string(&SuccessResponse {
                            id: msg.request.id,
                            result: ResponseResult::PaneRead {
                                read: crate::api::schema::PaneReadResult {
                                    pane_id: "pane_1".into(),
                                    workspace_id: "ws_1".into(),
                                    tab_id: "tab_1".into(),
                                    source: crate::api::schema::ReadSource::RecentUnwrapped,
                                    format: crate::api::schema::ReadFormat::Text,
                                    text: String::new(),
                                    revision: 0,
                                    truncated: false,
                                },
                            },
                        })
                        .unwrap(),
                    )
                    .unwrap();
            }
        });

        let (mut client, server, _path) = local_stream_pair("api-wait-disconnect");
        client
            .write_all(br#"{"id":"req_wait","method":"pane.wait_for_output","params":{"pane_id":"pane_1","source":"recent","match":{"type":"substring","value":"never"}}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let result = handle_connection(server, &api_tx, &event_hub, &server_running, None);
            done_tx.send(result).unwrap();
        });

        first_read_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(client);

        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok());

        server_thread.join().unwrap();
        drop(running);
        responder.join().unwrap();
    }

    #[test]
    fn invalid_requests_preserve_only_unambiguous_string_ids() {
        let cases = [
            (
                r#"{"id":"mine","method":"pane.report_agent","params":{"pane_id":"w1:p1","status":"working","source":"x"}}"#,
                "mine",
            ),
            (r#"{"id":"escaped\"id","method":"unknown"}"#, "escaped\"id"),
            (r#"{"method":"unknown","params":{"id":"nested"}}"#, ""),
            (r#"{"id":123,"method":"unknown"}"#, ""),
            (
                r#"{"id":"first","id":"second","method":"ping","params":{}}"#,
                "",
            ),
            (r#"{"id":"truncated","method":"ping""#, ""),
            (r#"["not-an-object"]"#, ""),
        ];
        for (request, expected_id) in cases {
            let (api_tx, mut api_rx) = mpsc::unbounded_channel();
            let (mut client, server, path) = local_stream_pair("invalid-request-id");
            writeln!(client, "{request}").unwrap();
            let running = Arc::new(AtomicBool::new(true));
            handle_connection(server, &api_tx, &EventHub::default(), &running, None).unwrap();

            let mut response = String::new();
            BufReader::new(client)
                .read_to_string(&mut response)
                .unwrap();
            let response: ErrorResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(response.id, expected_id, "{request}");
            assert_eq!(response.error.code, "invalid_request");
            assert!(response.error.message.starts_with("invalid request: "));
            assert!(
                api_rx.try_recv().is_err(),
                "invalid requests must not dispatch"
            );
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn subscription_setup_errors_preserve_request_id_and_reject_entire_stream() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let event_hub = EventHub::default();
        let responder_event_hub = event_hub.clone();
        let responder = std::thread::spawn(move || {
            let msg = api_rx.blocking_recv().unwrap();
            let Method::PaneGet(params) = msg.request.method else {
                panic!("unexpected request: {:?}", msg.request.method);
            };
            assert_eq!(params.pane_id, "w999:p9");
            responder_event_hub.push(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneClosed,
                data: crate::api::schema::EventData::PaneClosed {
                    pane_id: "w999:p9".into(),
                    workspace_id: "w999".into(),
                },
            });
            msg.respond_to
                .send(error_response_json(
                    msg.request.id,
                    "pane_not_found",
                    "pane w999:p9 not found".into(),
                ))
                .unwrap();
            assert!(
                api_rx.blocking_recv().is_none(),
                "rejection must not start polling"
            );
        });
        let (mut client, server, path) = local_stream_pair("subscription-error-id");
        let request = r#"{"id":"panefold:events","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"},{"type":"pane.closed"},{"type":"pane.agent_status_changed","pane_id":"w999:p9"}]}}"#;
        writeln!(client, "{request}").unwrap();
        let running = Arc::new(AtomicBool::new(true));
        handle_connection(server, &api_tx, &event_hub, &running, None).unwrap();
        drop(api_tx);
        responder.join().unwrap();

        let mut response = String::new();
        BufReader::new(client)
            .read_to_string(&mut response)
            .unwrap();
        let response: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.id, "panefold:events");
        assert_eq!(response.error.code, "pane_not_found");
        assert_eq!(response.error.message, "pane w999:p9 not found");
        fs::remove_file(path).unwrap();
    }

    /// Serves `stream_subscriptions_with_heartbeat` for `params` on a local
    /// socket and forwards each client-side line to a channel.
    struct SubscriptionStreamHarness {
        lines: std::sync::mpsc::Receiver<serde_json::Value>,
        running: Arc<AtomicBool>,
        server: std::thread::JoinHandle<()>,
        path: PathBuf,
    }

    impl SubscriptionStreamHarness {
        fn start(
            name: &str,
            params: serde_json::Value,
            api_tx: ApiRequestSender,
            event_hub: &EventHub,
            heartbeat_interval: Duration,
        ) -> Self {
            let params: crate::api::schema::EventsSubscribeParams =
                serde_json::from_value(params).unwrap();
            let (client, server, path) = local_stream_pair(name);
            let running = Arc::new(AtomicBool::new(true));
            let server_running = Arc::clone(&running);
            let server_hub = event_hub.clone();
            let server = std::thread::spawn(move || {
                stream_subscriptions_with_heartbeat(
                    ApiStream::Local(server),
                    "sub".into(),
                    params,
                    &api_tx,
                    &server_hub,
                    &server_running,
                    heartbeat_interval,
                )
                .unwrap();
            });
            let (line_tx, lines) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(client).lines() {
                    let Ok(line) = line else { return };
                    if line_tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                        return;
                    }
                }
            });
            Self {
                lines,
                running,
                server,
                path,
            }
        }

        fn next(&self) -> serde_json::Value {
            self.lines
                .recv_timeout(Duration::from_secs(5))
                .expect("subscription line")
        }

        /// The server closed the stream with no further line.
        fn closed(&self) -> bool {
            matches!(
                self.lines.recv_timeout(Duration::from_secs(5)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
            )
        }

        fn stop(self) {
            self.running.store(false, Ordering::Relaxed);
            self.server.join().unwrap();
            let _ = fs::remove_file(self.path);
        }
    }

    fn focused_event(workspace_id: &str) -> crate::api::schema::EventEnvelope {
        crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::WorkspaceFocused,
            data: crate::api::schema::EventData::WorkspaceFocused {
                workspace_id: workspace_id.into(),
            },
        }
    }

    fn status_event(
        pane_id: &str,
        agent_status: crate::api::schema::AgentStatus,
    ) -> crate::api::schema::EventEnvelope {
        crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::PaneAgentStatusChanged,
            data: crate::api::schema::EventData::PaneAgentStatusChanged {
                pane_id: pane_id.into(),
                workspace_id: "ws_1".into(),
                agent_status,
                input_pending: false,
                input_prompt_kind: None,
                agent: Some("pi".into()),
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
                turn: None,
                turn_epoch: None,
            },
        }
    }

    /// Answers `pane.list` with one idle pane and `pane.get` for `pane_1`. The
    /// first `pane.get` after the subscribe probe reports that it is blocked
    /// and waits for the test to release it.
    fn spawn_gated_pane_responder() -> (
        ApiRequestSender,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (blocked_tx, blocked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let mut pane_gets = 0;
            while let Some(msg) = api_rx.blocking_recv() {
                let result = match msg.request.method {
                    Method::PaneList(_) => ResponseResult::PaneList {
                        panes: vec![pane_info("pane_1", crate::api::schema::AgentStatus::Idle)],
                    },
                    Method::PaneGet(params) if params.pane_id == "pane_1" => {
                        pane_gets += 1;
                        if pane_gets == 2 {
                            let _ = blocked_tx.send(());
                            let _ = release_rx.recv();
                        }
                        ResponseResult::PaneInfo {
                            pane: pane_info("pane_1", crate::api::schema::AgentStatus::Idle),
                        }
                    }
                    Method::PaneGet(params) => {
                        let _ = msg.respond_to.send(error_response_json(
                            msg.request.id,
                            "pane_not_found",
                            format!("pane {} not found", params.pane_id),
                        ));
                        continue;
                    }
                    other => panic!("unexpected request: {other:?}"),
                };
                let _ = msg.respond_to.send(
                    serde_json::to_string(&SuccessResponse {
                        id: msg.request.id,
                        result,
                    })
                    .unwrap(),
                );
            }
        });
        (api_tx, blocked_rx, release_tx)
    }

    #[test]
    fn all_panes_status_subscription_reports_pane_created_after_it_started() {
        let (api_tx, _blocked, _release) = spawn_gated_pane_responder();
        let event_hub = EventHub::default();
        let stream = SubscriptionStreamHarness::start(
            "sub-all-panes",
            serde_json::json!({"subscriptions": [
                {"type": "pane.agent_status_changed", "agent_status": "working"},
                {"type": "pane.turn_completed"}
            ]}),
            api_tx,
            &event_hub,
            SUBSCRIPTION_HEARTBEAT_INTERVAL,
        );
        assert_eq!(stream.next()["result"]["type"], "subscription_started");

        event_hub.push(status_event(
            "pane_2",
            crate::api::schema::AgentStatus::Idle,
        ));
        event_hub.push(status_event(
            "pane_2",
            crate::api::schema::AgentStatus::Working,
        ));
        event_hub.push(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::PaneTurnCompleted,
            data: crate::api::schema::EventData::PaneTurnCompleted {
                pane: pane_info("pane_3", crate::api::schema::AgentStatus::Done),
                turn: 1,
                turn_epoch: 9,
                outcome: crate::terminal::TurnOutcome::Completed,
                message: None,
                message_truncated: false,
                agent_session_path: None,
                completed_unix_ms: 1,
            },
        });

        let status = stream.next();
        assert_eq!(status["event"], "pane.agent_status_changed");
        assert_eq!(status["data"]["pane_id"], "pane_2");
        assert_eq!(status["data"]["agent_status"], "working");
        assert_eq!(status["seq"], 2);
        let turn = stream.next();
        assert_eq!(turn["event"], "pane.turn_completed");
        assert_eq!(turn["data"]["pane"]["pane_id"], "pane_3");
        assert_eq!(turn["seq"], 3);
        stream.stop();
    }

    #[test]
    fn subscription_stream_delivers_a_burst_complete_and_in_hub_order() {
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let event_hub = EventHub::default();
        let stream = SubscriptionStreamHarness::start(
            "sub-burst",
            serde_json::json!({"subscriptions": [
                {"type": "workspace.created"},
                {"type": "workspace.focused"},
                {"type": "pane.closed"}
            ]}),
            api_tx,
            &event_hub,
            SUBSCRIPTION_HEARTBEAT_INTERVAL,
        );
        assert_eq!(stream.next()["result"]["type"], "subscription_started");

        const BURST: usize = 200;
        for index in 0..BURST {
            if index % 2 == 0 {
                event_hub.push(focused_event(&format!("w{index}")));
            } else {
                event_hub.push(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::PaneClosed,
                    data: crate::api::schema::EventData::PaneClosed {
                        pane_id: format!("p{index}"),
                        workspace_id: "w".into(),
                    },
                });
            }
        }

        for index in 0..BURST {
            let line = stream.next();
            assert_eq!(line["seq"], index as u64 + 1, "line {index}: {line}");
            let id = if index % 2 == 0 {
                &line["data"]["workspace_id"]
            } else {
                &line["data"]["pane_id"]
            };
            assert!(id.as_str().unwrap().ends_with(&index.to_string()), "{line}");
        }
        stream.stop();
    }

    const RING_OVERFLOW_EVENTS: usize = 5_000;

    /// Stalls the stream inside a `pane.get` while the ring overflows, then
    /// releases it with the subscriber's cursor already evicted from history.
    fn stream_after_ring_overflow(events_v2: bool) -> (SubscriptionStreamHarness, EventHub) {
        let (api_tx, blocked, release) = spawn_gated_pane_responder();
        let event_hub = EventHub::default();
        let stream = SubscriptionStreamHarness::start(
            if events_v2 {
                "sub-lag-v2"
            } else {
                "sub-lag-v1"
            },
            serde_json::json!({
                "subscriptions": [
                    {"type": "workspace.focused"},
                    {"type": "pane.agent_status_changed", "pane_id": "pane_1"}
                ],
                "events_v2": events_v2,
            }),
            api_tx,
            &event_hub,
            SUBSCRIPTION_HEARTBEAT_INTERVAL,
        );
        assert_eq!(
            stream.next(),
            serde_json::json!({"id": "sub", "result": {"type": "subscription_started"}})
        );
        blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        for index in 0..RING_OVERFLOW_EVENTS {
            event_hub.push(focused_event(&index.to_string()));
        }
        release.send(()).unwrap();
        (stream, event_hub)
    }

    #[test]
    fn lagged_v2_subscription_reports_missed_range_then_continues() {
        let (stream, event_hub) = stream_after_ring_overflow(true);
        let mut lines = vec![stream.next()];
        while lines.last().unwrap()["seq"] != RING_OVERFLOW_EVENTS as u64 {
            lines.push(stream.next());
        }
        event_hub.push(focused_event("after"));
        lines.push(stream.next());
        stream.stop();

        let first_retained = (RING_OVERFLOW_EVENTS - EventHub::MAX_EVENTS) as u64 + 1;
        assert_eq!(
            lines[0],
            serde_json::json!({
                "control": "lagged",
                "seq": first_retained - 1,
                "first_missed_seq": 1,
                "last_missed_seq": first_retained - 1,
            })
        );
        let events = &lines[1..];
        assert_eq!(events.len(), EventHub::MAX_EVENTS + 1);
        assert!(events
            .iter()
            .zip(first_retained..)
            .all(|(line, seq)| line["seq"] == seq && line["event"] == "workspace_focused"));
        assert_eq!(events.last().unwrap()["data"]["workspace_id"], "after");
    }

    #[test]
    fn lagged_legacy_subscription_reports_events_lost_and_closes() {
        let (stream, _event_hub) = stream_after_ring_overflow(false);
        let line = stream.next();
        assert_eq!(line["id"], "sub", "{line}");
        assert_eq!(line["error"]["code"], "events_lost", "{line}");
        assert!(
            stream.closed(),
            "a legacy subscriber must not see a partial stream after events_lost"
        );
        stream.stop();
    }

    #[test]
    fn idle_v2_subscription_writes_heartbeats_and_legacy_stays_silent() {
        let event_hub = EventHub::default();
        event_hub.push(focused_event("before"));
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let v2 = SubscriptionStreamHarness::start(
            "sub-heartbeat-v2",
            serde_json::json!({"subscriptions": [{"type": "workspace.created"}], "events_v2": true}),
            api_tx.clone(),
            &event_hub,
            Duration::from_millis(150),
        );
        let legacy = SubscriptionStreamHarness::start(
            "sub-heartbeat-v1",
            serde_json::json!({"subscriptions": [{"type": "workspace.created"}]}),
            api_tx,
            &event_hub,
            Duration::from_millis(150),
        );
        assert_eq!(v2.next()["result"]["type"], "subscription_started");
        assert_eq!(legacy.next()["result"]["type"], "subscription_started");

        let heartbeat = serde_json::json!({"control": "heartbeat", "seq": 1});
        assert_eq!(v2.next(), heartbeat);
        assert_eq!(v2.next(), heartbeat);
        assert!(legacy
            .lines
            .recv_timeout(Duration::from_millis(500))
            .is_err());
        v2.stop();
        legacy.stop();
    }

    #[test]
    fn v2_subscription_skips_missing_pane_entries_and_keeps_the_rest() {
        let (api_tx, _blocked, _release) = spawn_gated_pane_responder();
        let event_hub = EventHub::default();
        let stream = SubscriptionStreamHarness::start(
            "sub-rejected",
            serde_json::json!({
                "subscriptions": [
                    {"type": "pane.turn_completed", "pane_id": "gone"},
                    {"type": "workspace.focused"}
                ],
                "events_v2": true,
            }),
            api_tx,
            &event_hub,
            SUBSCRIPTION_HEARTBEAT_INTERVAL,
        );
        let ack = stream.next();
        assert_eq!(ack["result"]["type"], "subscription_started");
        assert_eq!(
            ack["result"]["rejected"],
            serde_json::json!([{"index": 0, "error": {"code": "pane_not_found", "message": "pane gone not found"}}])
        );
        event_hub.push(focused_event("w1"));
        assert_eq!(stream.next()["data"]["workspace_id"], "w1");
        stream.stop();
    }

    #[test]
    fn ping_advertises_events_v2() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let response = handle_request(
            Request {
                id: "req_1".into(),
                method: Method::Ping(crate::api::schema::PingParams::default()),
            },
            &tx,
            default_capabilities(),
            None,
            None,
            None,
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["capabilities"]["events_v2"], true);
    }

    #[test]
    fn ping_advertises_api_bridge_multi_on_unix() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let response = handle_request(
            Request {
                id: "req_1".into(),
                method: Method::Ping(crate::api::schema::PingParams::default()),
            },
            &tx,
            default_capabilities(),
            None,
            None,
            None,
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["result"]["capabilities"]["api_bridge_multi"],
            cfg!(unix)
        );
    }

    #[test]
    fn subscriptions_stop_when_client_disconnects() {
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-sub-disconnect");
        client
            .write_all(
                br#"{"id":"sub_1","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"}]}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let result = handle_connection(server, &api_tx, &event_hub, &server_running, None);
            done_tx.send(result).unwrap();
        });

        let ack = read_line(&mut client);
        let ack: serde_json::Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(ack["result"]["type"], "subscription_started");
        assert_eq!(ack["id"], "sub_1");

        drop(client);

        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok());
        server_thread.join().unwrap();
    }

    #[test]
    fn subscriptions_stop_when_server_shuts_down() {
        let (api_tx, _api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-sub-shutdown");
        client
            .write_all(
                br#"{"id":"sub_2","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"}]}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let result = handle_connection(server, &api_tx, &event_hub, &server_running, None);
            done_tx.send(result).unwrap();
        });

        let ack = read_line(&mut client);
        let ack: serde_json::Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(ack["result"]["type"], "subscription_started");

        running.store(false, Ordering::Relaxed);

        let result = done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok());
        server_thread.join().unwrap();
    }

    #[test]
    fn local_connection_bypasses_federation_capability_gate() {
        use std::io::Write as _;

        // A local (unix-socket) connection carries `federation: None`, so even a
        // method federation denies outright (here `pane.close`) must dispatch
        // unfiltered. This is the regression guard that the Part-1 gate never
        // touches the local control path.
        let (mut client, server, path) = local_stream_pair("local-no-fed-filter");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel();
        let running = Arc::new(AtomicBool::new(true));
        let handle = std::thread::spawn(move || {
            let _ = handle_connection_with_stop(
                ApiStream::Local(server),
                &api_tx,
                &EventHub::default(),
                &running,
                None,
                None,
                // Local path: no federation peer, so no capability filtering.
                None,
                &HashMap::new(),
                #[cfg(unix)]
                None,
            );
        });

        let request = serde_json::to_string(&Request {
            id: "local_close".into(),
            method: Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: "pane_1".into(),
            }),
        })
        .unwrap();
        client.write_all(request.as_bytes()).unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        // The normally-denied method reached the app dispatch path unfiltered.
        let message = api_rx.blocking_recv().expect("request dispatched to app");
        assert!(matches!(message.request.method, Method::PaneClose(_)));

        // Dropping the message disconnects the response channel so the connection
        // thread unwinds instead of blocking on a reply that never comes.
        drop(message);
        let _ = handle.join();
        let _ = std::fs::remove_file(path);
    }
}

/// Federation TCP listener + token-gate tests. These run a real loopback
/// `TcpListener` through [`spawn_federation_listener`] and drive it with the
/// real [`ApiClient`] TCP transport (or a raw socket for the rejection paths),
/// entirely in-process — no external machine.
#[cfg(test)]
mod federation_tests {
    use super::*;
    use crate::api::client::{ApiClient, ConnectionTarget};
    use crate::api::federation_relay::{PeerRelay, RelayTiming};
    use crate::api::schema::{
        AgentInfo, AgentPromptDelivery, AgentPromptParams, AgentRenameParams, AgentStartParams,
        AgentStatus, EmptyParams, EventData, EventEnvelope, EventKind, EventsSubscribeParams,
        PaneTarget, PingParams, Subscription,
    };
    use crate::config::CapabilityTier;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpStream};
    use tokio::sync::mpsc;

    fn presentation(alias: &str) -> PeerPresentation {
        PeerPresentation {
            profile_id: None,
            label: alias.into(),
        }
    }

    fn shared_presentation(alias: &str) -> Arc<RwLock<PeerPresentation>> {
        Arc::new(RwLock::new(presentation(alias)))
    }

    /// A running federation listener bound to a loopback ephemeral port, plus the
    /// app channel receiver so a test can assert whether any request reached the
    /// dispatch path. Dropping it stops and joins the accept thread.
    struct TestFederation {
        addr: SocketAddr,
        event_hub: EventHub,
        running: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
        _api_tx: ApiRequestSender,
        api_rx: mpsc::UnboundedReceiver<ApiRequestMessage>,
    }

    impl Drop for TestFederation {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn peer_ctx(alias: &str, tier: CapabilityTier) -> PeerContext {
        PeerContext {
            alias: alias.into(),
            tier,
            expected_node_id: None,
        }
    }

    /// A peer bound to `token` at `tier` with a pinned `expected_node_id`.
    fn one_peer_pinned(
        token: &str,
        tier: CapabilityTier,
        node_id: &str,
    ) -> Vec<(String, PeerContext)> {
        vec![(
            token.to_string(),
            PeerContext {
                alias: "peer".into(),
                tier,
                expected_node_id: Some(node_id.to_string()),
            },
        )]
    }

    /// One peer bound to `token` at `tier`, in the shape `spawn_federation_listener`
    /// expects.
    fn one_peer(token: &str, tier: CapabilityTier) -> Vec<(String, PeerContext)> {
        vec![(token.to_string(), peer_ctx("peer", tier))]
    }

    fn start_federation(peers: Vec<(String, PeerContext)>) -> TestFederation {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback federation listener");
        let addr = listener.local_addr().expect("federation listener addr");
        let (api_tx, api_rx) = mpsc::unbounded_channel();
        let event_hub = EventHub::default();
        let running = Arc::new(AtomicBool::new(true));
        let thread = spawn_federation_listener(
            listener,
            peers,
            api_tx.clone(),
            event_hub.clone(),
            None,
            Arc::clone(&running),
            None,
        )
        .expect("spawn federation listener");
        TestFederation {
            addr,
            event_hub,
            running,
            thread: Some(thread),
            _api_tx: api_tx,
            api_rx,
        }
    }

    fn tcp_client(addr: SocketAddr, token: Option<&str>) -> ApiClient {
        ApiClient::for_target(ConnectionTarget::Tcp {
            addr,
            token: token.map(str::to_owned),
        })
    }

    /// Open a raw federation connection and send `token`'s hello, returning the
    /// still-open stream so the caller can drive requests on it directly.
    fn raw_hello(addr: SocketAddr, token: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).expect("connect");
        let hello = FederationHello::new(token).to_line().unwrap();
        writeln!(stream, "{hello}").unwrap();
        stream.flush().unwrap();
        stream
    }

    /// Like [`raw_hello`], but stamps an explicit `machine_id` so tests can drive
    /// the `expected_node_id` identity pin with a value they control (the real
    /// `ApiClient` TCP path would send this process's persisted install id).
    fn raw_hello_with_machine_id(addr: SocketAddr, token: &str, machine_id: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).expect("connect");
        let hello = FederationHello::new(token)
            .with_machine_id(machine_id.to_string())
            .to_line()
            .unwrap();
        writeln!(stream, "{hello}").unwrap();
        stream.flush().unwrap();
        stream
    }

    fn send_request(stream: &mut TcpStream, id: &str, method: Method) {
        let encoded = serde_json::to_string(&Request {
            id: id.into(),
            method,
        })
        .unwrap();
        writeln!(stream, "{encoded}").unwrap();
        stream.flush().unwrap();
    }

    fn read_json_line(stream: &TcpStream) -> serde_json::Value {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response line");
        serde_json::from_str(&line).expect("response is json")
    }

    #[test]
    fn valid_token_round_trips_a_ping_over_tcp() {
        let fed = start_federation(one_peer("s3cret", CapabilityTier::Observe));
        let client = tcp_client(fed.addr, Some("s3cret"));

        let response = client
            .request_value(&Request {
                id: "fed_ping".into(),
                method: Method::Ping(PingParams::default()),
            })
            .expect("federation ping round-trips");

        assert_eq!(response["id"], "fed_ping");
        assert_eq!(response["result"]["type"], "pong");
    }

    #[test]
    fn inbound_peer_target_naming_a_peer_is_not_transit_proxied() {
        // Confused-deputy guard. A FEDERATION-INBOUND peer must never drive the
        // home's OWN outbound routing: the home drives its peers only from LOCAL
        // clients, never by relaying an inbound peer through to its other peers
        // on the home's own credentials. So an inbound Observe peer sending a
        // target that names another peer (`remote/screen`) must fall through to
        // LOCAL dispatch with the target UNREWRITTEN — never split, stripped, and
        // proxied onward. (The local-client side that DOES route is covered by
        // `proxy_returns_the_peers_read_snapshot_verbatim`.) The inbound listener
        // is given no outbound registry at all, so this holds by construction.
        let mut fed = start_federation(one_peer("peertok", CapabilityTier::Observe));

        // App responder: proves the request reached local dispatch, and captures
        // the target it arrived with. A transit-proxied request would never have
        // reached the app at all.
        let mut api_rx = std::mem::replace(&mut fed.api_rx, mpsc::unbounded_channel().1);
        let responder = std::thread::spawn(move || {
            for _ in 0..300 {
                if let Ok(msg) = api_rx.try_recv() {
                    let target = match &msg.request.method {
                        Method::AgentRead(params) => params.target.clone(),
                        other => panic!("unexpected local method: {other:?}"),
                    };
                    let resp = error_response_json(
                        msg.request.id.clone(),
                        "not_found",
                        "no such agent".into(),
                    );
                    let _ = msg.respond_to.send(resp);
                    return Some(target);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        });

        let mut stream = raw_hello(fed.addr, "peertok");
        send_request(
            &mut stream,
            "ir1",
            Method::AgentRead(crate::api::schema::AgentReadParams {
                target: "remote/screen".into(),
                source: crate::api::schema::ReadSource::Visible,
                lines: None,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
            }),
        );
        let response = read_json_line(&stream);
        assert_eq!(response["id"], "ir1");
        assert_eq!(
            response["error"]["code"], "not_found",
            "an inbound `<alias>/…` target must resolve locally (not-found), not be proxied"
        );

        let seen_target = responder
            .join()
            .expect("responder thread panicked")
            .expect("inbound request never reached local dispatch (it was transit-proxied)");
        assert_eq!(
            seen_target, "remote/screen",
            "an inbound peer's target must reach local dispatch UNREWRITTEN, never split and proxied"
        );
    }

    #[test]
    fn wrong_token_is_rejected_and_never_dispatches() {
        let mut fed = start_federation(one_peer("right", CapabilityTier::Observe));

        // A bad hello is rejected before the connection handler ever reads a
        // request line, so nothing can reach the app dispatch path.
        let mut stream = TcpStream::connect(fed.addr).expect("connect");
        let hello = FederationHello::new("wrong").to_line().unwrap();
        writeln!(stream, "{hello}").unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read rejection line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("rejection is json");
        assert_eq!(value["error"]["code"], "unauthorized");

        // The connection is closed right after the error line (clean EOF: the
        // whole hello was consumed, so there is no unread data to force an RST).
        let mut rest = String::new();
        assert_eq!(
            reader.read_line(&mut rest).expect("read after close"),
            0,
            "connection was not closed after the rejection"
        );

        // No request ever reached the app dispatch path.
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            fed.api_rx.try_recv().is_err(),
            "a rejected connection dispatched a request"
        );
    }

    #[test]
    fn missing_hello_is_rejected() {
        let fed = start_federation(one_peer("right", CapabilityTier::Observe));

        // A request line where the hello is expected is not a hello.
        let mut stream = TcpStream::connect(fed.addr).expect("connect");
        writeln!(stream, r#"{{"id":"x","method":"ping","params":{{}}}}"#).unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read rejection line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("rejection is json");
        assert_eq!(value["error"]["code"], "unauthorized");

        let mut rest = String::new();
        assert_eq!(
            reader.read_line(&mut rest).expect("read after close"),
            0,
            "connection was not closed after the missing-hello rejection"
        );
    }

    #[test]
    fn request_stream_yields_multiple_lines_then_terminates_on_close() {
        let fed = start_federation(one_peer("s3cret", CapabilityTier::Observe));
        let client = tcp_client(fed.addr, Some("s3cret"));

        let mut lines = client
            .request_stream(&Request {
                id: "fed_sub".into(),
                method: Method::EventsSubscribe(EventsSubscribeParams {
                    subscriptions: vec![Subscription::PaneClosed {}],
                    events_v2: false,
                    local_only: false,
                }),
            })
            .expect("open subscription stream");

        // Line 1: the subscription ack.
        let ack = lines
            .next()
            .expect("subscription ack line")
            .expect("ack is io-ok");
        assert_eq!(ack["result"]["type"], "subscription_started");

        // Push a matching event; line 2 is that event.
        fed.event_hub.push(EventEnvelope {
            event: EventKind::PaneClosed,
            data: EventData::PaneClosed {
                pane_id: "pane_1".into(),
                workspace_id: "ws_1".into(),
            },
        });
        let event = lines
            .next()
            .expect("streamed event line")
            .expect("event is io-ok");
        assert_eq!(event["event"], "pane_closed");
        assert_eq!(event["data"]["pane_id"], "pane_1");

        // Shutting the server down closes the connection; the stream terminates.
        fed.running.store(false, Ordering::Relaxed);
        assert!(
            lines.next().is_none(),
            "stream did not terminate when the connection closed"
        );
    }

    #[test]
    fn empty_token_set_never_authorizes() {
        // A listener started with no tokens must reject even an empty-token hello
        // (the production bind path refuses to start such a listener at all).
        let fed = start_federation(Vec::new());
        let mut stream = TcpStream::connect(fed.addr).expect("connect");
        let hello = FederationHello::new("").to_line().unwrap();
        writeln!(stream, "{hello}").unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read rejection line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("rejection is json");
        assert_eq!(value["error"]["code"], "unauthorized");
    }

    /// Poll the app channel briefly for a dispatched request, panicking if none
    /// arrives. Used to prove that a permitted federated method reached the
    /// dispatch path (as opposed to being gate-filtered).
    fn recv_dispatched(
        api_rx: &mut mpsc::UnboundedReceiver<ApiRequestMessage>,
    ) -> ApiRequestMessage {
        for _ in 0..200 {
            if let Ok(message) = api_rx.try_recv() {
                return message;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no request reached the app dispatch path within the timeout");
    }

    #[test]
    fn every_wire_method_resolves_to_a_deliberate_access() {
        use CapabilityTier::{Admin, Interact, Observe};
        use FederationAccess::{AllowedAt, Denied};

        // The complete, deliberate classification of every wire-reachable API
        // method. Default-deny means a method added to `Method` without a decision
        // here still resolves to `Denied` (safe), but this table is the audit of
        // exactly what federation exposes and must be updated whenever a method's
        // federation exposure changes.
        let expected: &[(&str, FederationAccess)] = &[
            ("ping", AllowedAt(Observe)),
            ("server.stop", Denied),
            ("server.live_handoff", Denied),
            ("machine.status", Denied),
            ("server.reload_config", Denied),
            ("server.staged_update", Denied),
            ("server.apply_staged_update", Denied),
            ("server.agent_manifests", Denied),
            ("server.reload_agent_manifests", Denied),
            ("server.ssh_agent.register", Denied),
            ("notification.show", Denied),
            ("notifications.register_device", Denied),
            ("notifications.unregister_device", Denied),
            ("notifications.register_activity", Denied),
            ("notifications.unregister_activity", Denied),
            ("notifications.status", Denied),
            ("gram.send", Denied),
            ("gram.post", Denied),
            ("gram.list", Denied),
            ("gram.grab", Denied),
            ("gram.mark_read", Denied),
            ("gram.delete", Denied),
            ("gram.upload_chunk", Denied),
            ("gram.upload.discard", Denied),
            ("gram.get_file", Denied),
            ("gram.get_file_chunk", Denied),
            ("gram.relay", Denied),
            ("gram.relay_status", Denied),
            ("guest.invite.create", Denied),
            ("guest.list", Denied),
            ("guest.revoke", Denied),
            ("guest.update", Denied),
            ("guest.audit", Denied),
            ("guest.agent_probe", Denied),
            ("client.window_title.set", Denied),
            ("client.window_title.clear", Denied),
            ("client_shell.surface.set", Denied),
            ("session.snapshot", AllowedAt(Observe)),
            ("workspace.create", Denied),
            ("workspace.list", AllowedAt(Observe)),
            ("workspace.get", AllowedAt(Observe)),
            ("workspace.focus", Denied),
            ("workspace.rename", Denied),
            ("workspace.move", Denied),
            ("workspace.move_block", Denied),
            ("workspace.report_metadata", Denied),
            ("workspace.close", Denied),
            ("worktree.list", AllowedAt(Observe)),
            ("worktree.create", Denied),
            ("worktree.open", Denied),
            ("worktree.remove", Denied),
            ("tab.create", Denied),
            ("command.invoke", Denied),
            ("tab.list", AllowedAt(Observe)),
            ("tab.get", AllowedAt(Observe)),
            ("tab.focus", Denied),
            ("tab.rename", Denied),
            ("tab.move", Denied),
            ("tab.close", Denied),
            ("agent.list", AllowedAt(Observe)),
            ("agent.get", AllowedAt(Observe)),
            ("agent.read", AllowedAt(Observe)),
            ("agent.transcript", AllowedAt(Observe)),
            ("agent.explain", AllowedAt(Observe)),
            ("agent.send_keys", AllowedAt(Interact)),
            ("agent.rename", AllowedAt(Admin)),
            ("agent.archive", Denied),
            ("agent.unarchive", Denied),
            ("agent.forget", Denied),
            ("agent.view.set", AllowedAt(Admin)),
            ("agent.view.clear", AllowedAt(Admin)),
            ("agent.focus", AllowedAt(Admin)),
            ("agent.start", Denied),
            ("agent.restart", AllowedAt(Admin)),
            ("agent.transfer_session", Denied),
            ("accounts.list", AllowedAt(Observe)),
            ("accounts.create", Denied),
            ("accounts.remove", Denied),
            ("agent.kinds", AllowedAt(Observe)),
            ("fs.list_dir", Denied),
            ("agent.prompt", AllowedAt(Interact)),
            ("agent.wait", AllowedAt(Observe)),
            ("agent.answer", AllowedAt(Admin)),
            ("pane.copy_motion", Denied),
            ("pane.copy_search", Denied),
            ("pane.edit_scrollback", Denied),
            ("pane.link.activate", Denied),
            ("pane.link.resolve", Denied),
            ("pane.scroll", Denied),
            ("pane.selection.read", Denied),
            ("pane.split", Denied),
            ("pane.swap", Denied),
            ("pane.move", Denied),
            ("pane.zoom", Denied),
            ("pane.layout", Denied),
            ("pane.process_info", AllowedAt(Observe)),
            ("layout.export", AllowedAt(Observe)),
            ("layout.apply", Denied),
            ("layout.set_split_ratio", Denied),
            ("pane.neighbor", AllowedAt(Observe)),
            ("pane.edges", AllowedAt(Observe)),
            ("pane.focus_direction", Denied),
            ("pane.resize", Denied),
            ("pane.set_pty_size", AllowedAt(Admin)),
            ("pane.list", AllowedAt(Observe)),
            ("pane.current", AllowedAt(Observe)),
            ("pane.get", AllowedAt(Observe)),
            ("pane.turns", AllowedAt(Observe)),
            ("pane.focus", Denied),
            ("pane.input.set", AllowedAt(Admin)),
            ("pane.rename", AllowedAt(Admin)),
            ("pane.send_text", AllowedAt(Interact)),
            ("pane.send_keys", AllowedAt(Interact)),
            ("pane.send_input", AllowedAt(Admin)),
            ("pane.read", AllowedAt(Observe)),
            ("pane.history", AllowedAt(Observe)),
            ("pane.clear", Denied),
            ("pane.stream", AllowedAt(Observe)),
            ("pane.stream.open", Denied),
            ("pane.stream.close", Denied),
            ("pane.pty_lease.release", Denied),
            ("pane.input.stream", Denied),
            ("pane.input.stream.open", Denied),
            ("gram.upload.stream", Denied),
            ("gram.upload.stream.open", Denied),
            ("pane.report_agent", Denied),
            ("pane.report_agent_session", Denied),
            ("pane.report_metadata", Denied),
            ("pane.clear_agent_authority", Denied),
            ("pane.release_agent", Denied),
            ("pane.close", Denied),
            ("popup.close", Denied),
            ("events.subscribe", AllowedAt(Observe)),
            ("events.wait", AllowedAt(Observe)),
            ("pane.wait_for_output", AllowedAt(Observe)),
            ("integration.install", Denied),
            ("integration.uninstall", Denied),
            ("integration.list", Denied),
            ("plugin.link", Denied),
            ("plugin.list", Denied),
            ("plugin.unlink", Denied),
            ("plugin.enable", Denied),
            ("plugin.disable", Denied),
            ("plugin.action.list", Denied),
            ("plugin.action.invoke", Denied),
            ("plugin.log.list", Denied),
            ("plugin.pane.open", Denied),
            ("plugin.pane.focus", Denied),
            ("plugin.pane.close", Denied),
            ("product_announcement.dismiss", Denied),
            ("release_notes.dismiss", Denied),
        ];

        for (name, access) in expected {
            assert_eq!(
                federation_access(name),
                *access,
                "method {name} is mis-classified for federation"
            );
        }

        let audit_names: std::collections::BTreeSet<&str> =
            expected.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            audit_names.len(),
            expected.len(),
            "the federation audit table lists a method name more than once"
        );

        // Tripwire part 1 — completeness, bound to the source of truth. Method names
        // come from the `define_operations!` table (`Method::OPERATIONS`), which also
        // generates the `Method` enum and `api_method_name`, so adding a wire method
        // makes this fail until a deliberate classification row is added here.
        assert_eq!(
            audit_names,
            wire_method_names(),
            "the federation audit table must classify every wire method exactly; \
             a wire method was added or removed without a deliberate federation access row"
        );

        // Tripwire part 2 — spelling/coverage of the serde-visible wire methods.
        // The `Method` schema omits the `#[schemars(skip)]` streaming internals,
        // so it is a subset of the audit, not the whole; but every name it does
        // carry must appear verbatim in the audit, catching a mistyped row that
        // part 1's count alone would miss.
        let wire_names = schema_wire_method_names();
        let missing: Vec<&str> = wire_names
            .iter()
            .filter(|name| !audit_names.contains(name.as_str()))
            .map(String::as_str)
            .collect();
        assert!(
            missing.is_empty(),
            "these serde-visible wire methods are unclassified in the federation audit: {missing:?}"
        );
    }

    /// Every wire method name, from the operations table that defines `Method`.
    fn wire_method_names() -> std::collections::BTreeSet<&'static str> {
        Method::OPERATIONS.iter().map(|op| op.name).collect()
    }

    /// The serde-visible wire method names, from the `Method` enum's schema.
    /// `Method` is an adjacently-tagged serde enum, so each schema variant
    /// carries its wire name as the `method` tag constant. `#[schemars(skip)]`
    /// variants (the streaming internals) are absent, so this is a subset of the
    /// full method set — see [`wire_method_names`] for the total.
    fn schema_wire_method_names() -> std::collections::BTreeSet<String> {
        let schema = schemars::schema_for!(Method);
        let value = serde_json::to_value(&schema).expect("method schema serializes to json");
        let variants = value
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .expect("Method schema is a tagged-enum `oneOf`");
        variants
            .iter()
            .map(|variant| {
                let method = variant
                    .get("properties")
                    .and_then(|properties| properties.get("method"))
                    .unwrap_or_else(|| {
                        panic!("Method schema variant has no method tag: {variant}")
                    });
                // schemars renders a single-value tag as `const`; tolerate an
                // `enum: [name]` rendering too so a schemars upgrade cannot
                // silently defeat the tripwire.
                let name = method
                    .get("const")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| {
                        method
                            .get("enum")
                            .and_then(serde_json::Value::as_array)
                            .and_then(|values| values.first())
                            .and_then(serde_json::Value::as_str)
                    })
                    .unwrap_or_else(|| panic!("method tag is not a string constant: {method}"));
                name.to_string()
            })
            .collect()
    }

    #[test]
    fn observe_peer_allows_reads_and_denies_writes() {
        let mut fed = start_federation(one_peer("obs", CapabilityTier::Observe));

        // agent.list (observe) reaches the app dispatch path.
        let mut reader = raw_hello(fed.addr, "obs");
        send_request(
            &mut reader,
            "list",
            Method::AgentList(crate::api::schema::AgentListParams::default()),
        );
        let message = recv_dispatched(&mut fed.api_rx);
        assert!(matches!(message.request.method, Method::AgentList(_)));
        drop(message);
        drop(reader);

        // agent.prompt (interact) is above an observe peer's tier: forbidden.
        let mut prompt = raw_hello(fed.addr, "obs");
        send_request(
            &mut prompt,
            "prompt",
            Method::AgentPrompt(AgentPromptParams {
                target: "a".into(),
                text: "hi".into(),
                wait: None,
            }),
        );
        let resp = read_json_line(&prompt);
        assert_eq!(resp["error"]["code"], "forbidden");
        assert_eq!(resp["id"], "prompt");

        // Methods federation denies outright are forbidden at any tier.
        for (id, method) in [
            (
                "start",
                Method::AgentStart(AgentStartParams {
                    name: "n".into(),
                    kind: "k".into(),
                    pane_id: "p".into(),
                    args: vec![],
                    timeout_ms: None,
                    account: None,
                }),
            ),
            ("stop", Method::ServerStop(EmptyParams {})),
            (
                "close",
                Method::PaneClose(PaneTarget {
                    pane_id: "p".into(),
                }),
            ),
        ] {
            let mut stream = raw_hello(fed.addr, "obs");
            send_request(&mut stream, id, method);
            assert_eq!(read_json_line(&stream)["error"]["code"], "forbidden");
        }

        // None of the forbidden calls reached the app dispatch path.
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            fed.api_rx.try_recv().is_err(),
            "a forbidden federated call reached the dispatch path"
        );
    }

    #[test]
    fn interact_peer_allows_prompt_and_denies_admin() {
        let mut fed = start_federation(one_peer("act", CapabilityTier::Interact));

        // agent.prompt (interact) is dispatched for an interact peer.
        let mut stream = raw_hello(fed.addr, "act");
        send_request(
            &mut stream,
            "prompt",
            Method::AgentPrompt(AgentPromptParams {
                target: "a".into(),
                text: "hi".into(),
                wait: None,
            }),
        );
        let message = recv_dispatched(&mut fed.api_rx);
        assert!(matches!(message.request.method, Method::AgentPrompt(_)));
        drop(message);

        // agent.rename (admin) is above its tier: forbidden.
        let mut stream = raw_hello(fed.addr, "act");
        send_request(
            &mut stream,
            "rename",
            Method::AgentRename(AgentRenameParams {
                target: "a".into(),
                name: Some("n".into()),
            }),
        );
        assert_eq!(read_json_line(&stream)["error"]["code"], "forbidden");
    }

    #[test]
    fn admin_federation_peers_are_forbidden_every_guest_method() {
        use crate::api::schema::{
            GuestAuditParams, GuestInviteCreateParams, GuestListParams, GuestRevokeParams,
            GuestUpdateParams,
        };
        let mut fed = start_federation(one_peer("adm", CapabilityTier::Admin));
        for method in [
            Method::GuestInviteCreate(GuestInviteCreateParams {
                target: "w1:p1".into(),
                name: "friend".into(),
                owner_name: "Jerry".into(),
                machine_label: "Mac".into(),
                ttl_secs: None,
                share_gram: true,
                machine: None,
            }),
            Method::GuestList(GuestListParams::default()),
            Method::GuestRevoke(GuestRevokeParams {
                guest_id: Some("g".into()),
                ..Default::default()
            }),
            Method::GuestUpdate(GuestUpdateParams {
                guest_id: "g".into(),
                share_gram: true,
                machine: None,
            }),
            Method::GuestAudit(GuestAuditParams::default()),
        ] {
            let mut stream = raw_hello(fed.addr, "adm");
            send_request(&mut stream, "guest", method);
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["error"]["code"], "forbidden", "{line}");
        }
        assert!(
            fed.api_rx.try_recv().is_err(),
            "no guest RPC reached the app"
        );
    }

    #[test]
    fn admin_peer_allows_admin_methods() {
        let mut fed = start_federation(one_peer("adm", CapabilityTier::Admin));

        let mut stream = raw_hello(fed.addr, "adm");
        send_request(
            &mut stream,
            "rename",
            Method::AgentRename(AgentRenameParams {
                target: "a".into(),
                name: Some("n".into()),
            }),
        );
        let message = recv_dispatched(&mut fed.api_rx);
        assert!(matches!(message.request.method, Method::AgentRename(_)));
        drop(message);
    }

    #[test]
    fn two_peers_are_each_bound_to_their_own_tier() {
        let mut fed = start_federation(vec![
            (
                "obs-tok".into(),
                peer_ctx("observer", CapabilityTier::Observe),
            ),
            (
                "adm-tok".into(),
                peer_ctx("administrator", CapabilityTier::Admin),
            ),
        ]);

        // The observe peer's token cannot rename.
        let mut stream = raw_hello(fed.addr, "obs-tok");
        send_request(
            &mut stream,
            "r1",
            Method::AgentRename(AgentRenameParams {
                target: "a".into(),
                name: Some("n".into()),
            }),
        );
        assert_eq!(read_json_line(&stream)["error"]["code"], "forbidden");

        // The admin peer's token, over the same listener, can.
        let mut stream = raw_hello(fed.addr, "adm-tok");
        send_request(
            &mut stream,
            "r2",
            Method::AgentRename(AgentRenameParams {
                target: "a".into(),
                name: Some("n".into()),
            }),
        );
        let message = recv_dispatched(&mut fed.api_rx);
        assert!(matches!(message.request.method, Method::AgentRename(_)));
        drop(message);
    }

    #[test]
    fn proto_version_mismatch_is_rejected_and_a_match_proceeds() {
        let fed = start_federation(one_peer("tok", CapabilityTier::Observe));

        // A hello the daemon does not speak is rejected with a distinct code and
        // never dispatches.
        let mut stream = TcpStream::connect(fed.addr).expect("connect");
        let mut hello = FederationHello::new("tok");
        hello.proto_version = FEDERATION_PROTOCOL_VERSION + 1;
        writeln!(stream, "{}", hello.to_line().unwrap()).unwrap();
        stream.flush().unwrap();
        let resp = read_json_line(&stream);
        assert_eq!(resp["error"]["code"], "federation_protocol_mismatch");

        // The matching version with a valid token proceeds to a pong.
        let client = tcp_client(fed.addr, Some("tok"));
        let response = client
            .request_value(&Request {
                id: "v_ok".into(),
                method: Method::Ping(PingParams::default()),
            })
            .expect("ping round-trips at the negotiated version");
        assert_eq!(response["result"]["type"], "pong");
    }

    #[test]
    fn expected_node_id_match_proceeds() {
        // A peer pinned to "node-A": a valid token whose hello carries the
        // matching machine_id passes the pin and reaches the app dispatch path.
        let mut fed = start_federation(one_peer_pinned("tok", CapabilityTier::Observe, "node-A"));
        let mut stream = raw_hello_with_machine_id(fed.addr, "tok", "node-A");
        send_request(
            &mut stream,
            "list",
            Method::AgentList(crate::api::schema::AgentListParams::default()),
        );
        let message = recv_dispatched(&mut fed.api_rx);
        assert!(matches!(message.request.method, Method::AgentList(_)));
    }

    #[test]
    fn expected_node_id_mismatch_is_rejected_and_never_dispatches() {
        // A peer pinned to "node-A": the right token but a hello carrying a
        // different machine_id is rejected with the SAME opaque unauthorized line
        // as a bad token (the failing check is never revealed), at handshake time
        // before any request can be read.
        let mut fed = start_federation(one_peer_pinned("tok", CapabilityTier::Observe, "node-A"));

        // Write the hello AND a real request in a SINGLE flush, before the handler
        // reads. This makes "never dispatches" a meaningful check: if the pin
        // failed to reject, the handler would go on to read this request and
        // dispatch it. Doing both in one write (rather than a second write after
        // the hello) is deliberate — a second write racing the handler's
        // rejection-close RSTs on macOS.
        let hello = FederationHello::new("tok")
            .with_machine_id("node-B".to_string())
            .to_line()
            .unwrap();
        let request = serde_json::to_string(&Request {
            id: "list".into(),
            method: Method::AgentList(crate::api::schema::AgentListParams::default()),
        })
        .unwrap();
        let mut stream = TcpStream::connect(fed.addr).expect("connect");
        write!(stream, "{hello}\n{request}\n").expect("write hello+request");
        stream.flush().expect("flush hello+request");
        // Bound the read: a correctly-rejecting handler closes fast, but a broken
        // pin that DISPATCHED the request would then block awaiting a response and
        // never write — the timeout makes the test fall through to the no-dispatch
        // assertion (which catches that regression) instead of hanging.
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("set read timeout");

        // The rejection SHOULD be the opaque unauthorized line. Read it
        // best-effort: the handler rejects at the pin without reading the request,
        // so it closes with that request still unread — which on macOS/BSD RSTs the
        // connection and can discard the line (and a broken pin instead times out
        // above). Either way the load-bearing assertion below is that nothing was
        // dispatched.
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) > 0 {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                assert_eq!(
                    value["error"]["code"], "unauthorized",
                    "the rejection line, when delivered, must be the opaque unauthorized error"
                );
            }
        }

        std::thread::sleep(Duration::from_millis(50));
        assert!(
            fed.api_rx.try_recv().is_err(),
            "a machine-id-mismatched connection dispatched a request sent behind the hello"
        );
    }

    #[test]
    fn peer_without_expected_node_id_ignores_machine_id() {
        // Back-compat: a peer with no pin admits any presented machine_id.
        let mut fed = start_federation(one_peer("tok", CapabilityTier::Observe));
        let mut stream = raw_hello_with_machine_id(fed.addr, "tok", "any-unpinned-id");
        send_request(
            &mut stream,
            "list",
            Method::AgentList(crate::api::schema::AgentListParams::default()),
        );
        let message = recv_dispatched(&mut fed.api_rx);
        assert!(matches!(message.request.method, Method::AgentList(_)));
    }

    #[test]
    fn connection_cap_refuses_beyond_max_and_releases_on_close() {
        use std::io::Read as _;

        let fed = start_federation(one_peer("tok", CapabilityTier::Observe));

        // Fill every slot. A slot is reserved as soon as the accept loop spawns
        // the handler, so these connections occupy the cap while their handlers
        // wait (indefinitely, within the handshake timeout) for a hello.
        let mut held: Vec<TcpStream> = Vec::new();
        for _ in 0..MAX_FEDERATION_CONNECTIONS {
            held.push(TcpStream::connect(fed.addr).expect("connect"));
        }
        // Let the accept loop take and count all of them.
        std::thread::sleep(Duration::from_millis(400));

        // The next connection is over the cap: the listener accepts then closes it
        // without writing a byte, so the client reads a clean EOF.
        let mut over = TcpStream::connect(fed.addr).expect("connect");
        over.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 1];
        assert!(
            matches!(over.read(&mut buf), Ok(0)),
            "a connection over the cap was not refused and closed"
        );

        // Closing one in-flight connection frees a slot; a fresh connection then
        // completes a full authenticated ping.
        held.pop();
        std::thread::sleep(Duration::from_millis(500));
        let client = tcp_client(fed.addr, Some("tok"));
        let response = client
            .request_value(&Request {
                id: "after_release".into(),
                method: Method::Ping(PingParams::default()),
            })
            .expect("a slot freed up after a connection closed");
        assert_eq!(response["result"]["type"], "pong");
    }

    #[test]
    fn accept_loop_treats_transient_errors_as_retryable() {
        // The federation listener must never die on a transient accept error;
        // only `WouldBlock` is the idle signal, everything else is retried.
        assert_eq!(
            classify_accept_error(&io::Error::from(io::ErrorKind::WouldBlock)),
            AcceptBackoff::Idle
        );
        for kind in [
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Other,
        ] {
            assert_eq!(
                classify_accept_error(&io::Error::from(kind)),
                AcceptBackoff::Retry,
                "{kind:?} should be retried, not fatal"
            );
        }
    }

    // ---- Outbound federation client (W3) ----

    fn seeded_agent(status: AgentStatus, name: &str) -> AgentInfo {
        serde_json::from_value(serde_json::json!({
            "terminal_id": "term-remote",
            "name": name,
            "agent_status": status,
            "workspace_id": "ws-remote",
            "tab_id": "tab-remote",
            "pane_id": "pane-remote",
            "focused": false,
            "revision": 1,
        }))
        .expect("seeded agent deserializes")
    }

    fn unique_token_file(token: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "herdr-fed-poll-token-{}-{}.txt",
            std::process::id(),
            nanos
        ));
        std::fs::write(&path, token).expect("write token file");
        path
    }

    /// Shared token every seeded loopback peer gates on. The per-peer token
    /// FILES are distinct paths (from `unique_token_file`) but hold this same
    /// value, so a peer can authenticate to any of them.
    const SEEDED_PEER_TOKEN: &str = "poll-secret";

    /// A live loopback federation peer that answers `agent.list` with one seeded
    /// working agent named `agent_name`. Used by the manager reconcile tests.
    struct SeededPeer {
        addr: std::net::SocketAddr,
        token_path: PathBuf,
        running: Arc<AtomicBool>,
        listener: JoinHandle<()>,
        responder: JoinHandle<()>,
        /// The peer's own workspaces, answered (after one re-exported entry) to
        /// every `workspace.list`; tests may change them between polls.
        workspaces: Arc<Mutex<Vec<crate::api::schema::WorkspaceInfo>>>,
        /// How many `workspace.list` requests the peer has answered.
        workspace_list_requests: Arc<AtomicUsize>,
    }

    /// A peer-local workspace as `workspace.list` reports it.
    fn peer_workspace(
        workspace_id: &str,
        number: usize,
        label: &str,
        status: AgentStatus,
    ) -> crate::api::schema::WorkspaceInfo {
        serde_json::from_value(serde_json::json!({
            "workspace_id": workspace_id,
            "number": number,
            "label": label,
            "focused": number == 1,
            "pane_count": 1,
            "tab_count": 1,
            "active_tab_id": format!("{workspace_id}:t1"),
            "agent_status": status,
        }))
        .expect("peer workspace deserializes")
    }

    impl SeededPeer {
        /// Bind a loopback listener + responder returning one local agent and
        /// one already-federated agent, as an older peer would after ignoring
        /// the `local_only` parameter.
        fn spawn(agent_name: &'static str) -> Self {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("bind loopback federation listener");
            let addr = listener.local_addr().expect("listener addr");
            let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
            let event_hub = EventHub::default();
            let running = Arc::new(AtomicBool::new(true));
            let workspaces = Arc::new(Mutex::new(vec![
                peer_workspace("ws-remote", 1, "api", AgentStatus::Working),
                peer_workspace("ws-docs", 2, "docs", AgentStatus::Idle),
            ]));
            let workspace_list_requests = Arc::new(AtomicUsize::new(0));
            let responder_workspaces = Arc::clone(&workspaces);
            let responder_requests = Arc::clone(&workspace_list_requests);
            let listener_thread = spawn_federation_listener(
                listener,
                one_peer(SEEDED_PEER_TOKEN, CapabilityTier::Observe),
                api_tx,
                event_hub,
                None,
                Arc::clone(&running),
                None,
            )
            .expect("spawn federation listener");
            let responder = std::thread::spawn(move || {
                while let Some(msg) = api_rx.blocking_recv() {
                    let response = match msg.request.method {
                        Method::AgentList(params) => {
                            assert!(
                                params.local_only,
                                "federation polls must not request an aggregated agent directory"
                            );
                            let mut reexported = seeded_agent(AgentStatus::Working, "nested");
                            reexported.machine_id = Some("other-peer".into());
                            serde_json::to_string(&SuccessResponse {
                                id: msg.request.id,
                                result: ResponseResult::AgentList {
                                    agents: vec![
                                        seeded_agent(AgentStatus::Working, agent_name),
                                        reexported,
                                    ],
                                    origin_machine_id: Some("machine-peer".into()),
                                    origin_boot_id: Some("boot-peer".into()),
                                    origin_version: Some("test".into()),
                                    origin_protocol: Some(crate::protocol::PROTOCOL_VERSION),
                                    origin_capabilities: None,
                                },
                            })
                            .expect("encode agent.list response")
                        }
                        Method::WorkspaceList(params) => {
                            assert!(
                                params.local_only,
                                "federation polls must not request aggregated workspaces"
                            );
                            responder_requests.fetch_add(1, Ordering::SeqCst);
                            // What an older coordinator that ignores `local_only`
                            // would add: a workspace it caches from its own peer.
                            let mut reexported =
                                peer_workspace("nested-ws", 9, "nested", AgentStatus::Idle);
                            reexported.machine_id = Some("other-peer".into());
                            let mut workspaces = responder_workspaces
                                .lock()
                                .expect("workspaces lock")
                                .clone();
                            workspaces.push(reexported);
                            serde_json::to_string(&SuccessResponse {
                                id: msg.request.id,
                                result: ResponseResult::WorkspaceList { workspaces },
                            })
                            .expect("encode workspace.list response")
                        }
                        _ => error_response_json(
                            msg.request.id,
                            "unexpected_dispatch",
                            "only agent.list and workspace.list are expected in this test".into(),
                        ),
                    };
                    let _ = msg.respond_to.send(response);
                }
            });
            let token_path = unique_token_file(SEEDED_PEER_TOKEN);
            Self {
                addr,
                token_path,
                running,
                listener: listener_thread,
                responder,
                workspaces,
                workspace_list_requests,
            }
        }

        /// Stop the listener + responder and remove the token file. Call AFTER the
        /// manager's poll threads are joined so any in-flight poll completes.
        fn shutdown(self) {
            self.running.store(false, Ordering::Relaxed);
            let _ = self.listener.join();
            let _ = self.responder.join();
            let _ = std::fs::remove_file(&self.token_path);
        }
    }

    /// Build an outbound `FederationPeer` targeting `addr` over loopback TCP.
    fn reachable_peer_on(
        addr: std::net::SocketAddr,
        alias: &str,
        token_path: &Path,
    ) -> FederationPeer {
        FederationPeer {
            alias: alias.into(),
            display_label: None,
            endpoint: Some(format!("tcp://{addr}")),
            profile_id: None,
            remote_session: None,
            token_file: Some(token_path.to_string_lossy().into_owned()),
            expected_node_id: None,
            capability: CapabilityTier::Observe,
        }
    }

    /// Poll the store until `alias` is cached `Reachable` with its first agent
    /// named `expected_name`, or a bounded ~3s wait elapses. Returns whether it
    /// converged.
    fn wait_for_cached(
        cache: &Arc<Mutex<FederationStore>>,
        alias: &str,
        expected_name: &str,
    ) -> bool {
        for _ in 0..300 {
            {
                let store = cache.lock().expect("cache lock");
                if let Some(entry) = store.peer(alias) {
                    if entry.reachability == Reachability::Reachable
                        && entry.agents.first().and_then(|a| a.name.as_deref())
                            == Some(expected_name)
                    {
                        return true;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// The manager requests local-only state, drops a legacy peer's already
    /// federated entry, caches the owned agent alias-prefixed and `Reachable`,
    /// rewrites all five identity fields plus `machine_id`, and registers a
    /// proxy route for it.
    #[test]
    fn federation_client_caches_alias_prefixed_reachable_agents() {
        let peer_srv = SeededPeer::spawn("builder");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );
        let mut peer = reachable_peer_on(peer_srv.addr, "home", &peer_srv.token_path);
        peer.expected_node_id = Some("machine-peer".into());
        manager.reconcile(&[peer]);
        assert_eq!(
            manager.live_aliases(),
            vec!["home".to_string()],
            "one endpoint peer → one poll thread"
        );

        // The first poll fires immediately, so the cache populates fast.
        assert!(
            wait_for_cached(&cache, "home", "home/builder"),
            "poll thread did not cache the reachable prefixed agent"
        );
        {
            let store = cache.lock().expect("cache lock");
            let agent = &store.peer("home").expect("home cached").agents[0];
            assert_eq!(agent.terminal_id, "home/term-remote");
            assert_eq!(agent.workspace_id, "home/ws-remote");
            assert_eq!(agent.tab_id, "home/tab-remote");
            assert_eq!(agent.pane_id, "home/pane-remote");
            assert_eq!(agent.machine_id.as_deref(), Some("home"));
            assert_eq!(agent.machine_label.as_deref(), Some("home"));
            assert_eq!(agent.origin_machine_id.as_deref(), Some("machine-peer"));
            assert_eq!(
                store
                    .peer("home")
                    .unwrap()
                    .observation
                    .validated_machine_id
                    .as_deref(),
                Some("machine-peer")
            );
            assert_eq!(
                store.peer("home").unwrap().observation.remote_protocol,
                Some(crate::protocol::PROTOCOL_VERSION)
            );
        }

        // The read/merge helper stamps a reachable peer as-is with the status.
        let merged = cache.lock().expect("cache lock").merged_agents();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].agent_status, AgentStatus::Working);
        assert_eq!(merged[0].reachability, Some(Reachability::Reachable));
        assert_eq!(merged[0].last_known_status, None);

        // The outbound proxy registry has a route for the peer.
        assert!(manager.registry_snapshot().contains_key("home"));

        // The live poll thread also fills the peer's workspaces after its first
        // successful agent poll (#243).
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let workspaces = cache.lock().expect("cache lock").merged_workspaces();
            if workspaces.len() == 2 {
                assert_eq!(workspaces[0].workspace_id, "home/ws-remote");
                assert_eq!(workspaces[1].workspace_id, "home/ws-docs");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "poll thread did not cache the peer's workspaces"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Teardown: join the poll thread (its in-flight poll completes against the
        // still-up listener), then stop the listener + responder.
        manager.join_all();
        peer_srv.shutdown();
    }

    #[test]
    fn peer_label_change_updates_presentation_without_reconnect() {
        let peer_srv = SeededPeer::spawn("builder");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );
        let mut peer = reachable_peer_on(peer_srv.addr, "home", &peer_srv.token_path);
        peer.display_label = Some("Before".into());
        manager.reconcile(std::slice::from_ref(&peer));
        assert!(wait_for_cached(&cache, "home", "home/builder"));
        assert_eq!(
            cache
                .lock()
                .expect("cache lock")
                .peer("home")
                .unwrap()
                .agents[0]
                .machine_label
                .as_deref(),
            Some("Before")
        );

        peer.display_label = Some("After".into());
        manager.reconcile(&[peer]);
        assert_eq!(
            manager.reap_and_count_pending(),
            0,
            "display-only rename must not retire the poll thread"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let updated = cache
                .lock()
                .expect("cache lock")
                .peer("home")
                .and_then(|entry| entry.agents.first())
                .and_then(|agent| agent.machine_label.as_deref())
                == Some("After");
            if updated {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "renamed label did not reach cached agents"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        manager.join_all();
        peer_srv.shutdown();
    }

    #[test]
    fn federation_client_rejects_changed_machine_identity() {
        let peer_srv = SeededPeer::spawn("builder");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let client = ApiClient::for_target(ConnectionTarget::Tcp {
            addr: peer_srv.addr,
            token: Some(SEEDED_PEER_TOKEN.into()),
        });
        let running = Arc::new(AtomicBool::new(true));
        let peer_stop = Arc::new(AtomicBool::new(false));
        let mut tracker = ReachabilityTracker::default();
        let route = PeerRoute::for_test_unvalidated(ConnectionTarget::Tcp {
            addr: peer_srv.addr,
            token: Some(SEEDED_PEER_TOKEN.into()),
        });
        // Validated by an earlier poll; the machine behind the endpoint then
        // changes.
        route.set_identity_validated(true);

        assert_eq!(
            poll_once_into_cache(
                &client,
                "home",
                Some("machine-before-reinstall"),
                Some(&route),
                &shared_presentation("home"),
                &cache,
                &mut tracker,
                &running,
                &peer_stop,
            ),
            Reachability::Degraded
        );
        assert!(
            cache.lock().expect("cache lock").merged_agents().is_empty(),
            "an identity mismatch must not publish the peer's agents"
        );
        assert!(
            !route.identity_validated(),
            "identity mismatch must disable proxy routing"
        );
        assert_eq!(
            cache
                .lock()
                .expect("cache lock")
                .peer("home")
                .unwrap()
                .last_error_class,
            Some(FederationPollErrorClass::IdentityMismatch)
        );

        running.store(false, Ordering::Relaxed);
        peer_srv.shutdown();
    }

    /// The poll→cache path degrades a peer that stops answering: two misses keep
    /// the last-known agents (`Degraded`), a third flips it `Unreachable`, and the
    /// read helper then stamps `Unknown` while preserving the last-known status —
    /// never a stale idle/done. Driven through `poll_once_into_cache` against a
    /// dead port so it needs no real 5s poll intervals.
    #[test]
    fn federation_client_flips_to_unreachable_and_stamps_unknown() {
        // Seed the cache as if a prior poll had succeeded with an idle agent.
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        {
            let mut store = cache.lock().expect("cache lock");
            store.set_peer(
                "home",
                PeerCacheEntry::reachable(
                    vec![prefix_remote_agent(
                        "home",
                        &presentation("home"),
                        seeded_agent(AgentStatus::Idle, "idler"),
                    )
                    .expect("local agent identity can be qualified")],
                    Instant::now(),
                ),
            );
        }

        // A guaranteed-closed unprivileged loopback port: every poll is refused.
        let dead_addr = {
            let probe = TcpListener::bind("127.0.0.1:0").expect("probe bind");
            probe.local_addr().expect("probe addr")
            // `probe` drops here, freeing the port so connects are refused.
        };
        let client = ApiClient::for_target(ConnectionTarget::Tcp {
            addr: dead_addr,
            token: Some("t".into()),
        });
        let running = Arc::new(AtomicBool::new(true));
        // A live (unset) per-peer stop flag: the under-lock guard must NOT skip
        // these writes.
        let peer_stop = Arc::new(AtomicBool::new(false));
        let mut tracker = ReachabilityTracker::default();

        // First two misses: Degraded. The last-known agents are retained in the
        // cache, but the read helper already stamps the surfaced status `Unknown`
        // (an honest-offline peer never shows a stale idle/done) with the real
        // status preserved in `last_known_status`.
        assert_eq!(
            poll_once_into_cache(
                &client,
                "home",
                None,
                None,
                &shared_presentation("home"),
                &cache,
                &mut tracker,
                &running,
                &peer_stop,
            ),
            Reachability::Degraded
        );
        assert_eq!(
            poll_once_into_cache(
                &client,
                "home",
                None,
                None,
                &shared_presentation("home"),
                &cache,
                &mut tracker,
                &running,
                &peer_stop,
            ),
            Reachability::Degraded
        );
        {
            let merged = cache.lock().expect("cache lock").merged_agents();
            assert_eq!(
                merged[0].agent_status,
                AgentStatus::Unknown,
                "a degraded peer must not surface a stale idle/done"
            );
            assert_eq!(merged[0].reachability, Some(Reachability::Degraded));
            assert_eq!(merged[0].last_known_status, Some(AgentStatus::Idle));
        }

        // Third miss: Unreachable → the read helper still stamps Unknown + last-known.
        assert_eq!(
            poll_once_into_cache(
                &client,
                "home",
                None,
                None,
                &shared_presentation("home"),
                &cache,
                &mut tracker,
                &running,
                &peer_stop,
            ),
            Reachability::Unreachable
        );
        let merged = cache.lock().expect("cache lock").merged_agents();
        assert_eq!(merged.len(), 1, "last-known agents retained across misses");
        assert_eq!(
            merged[0].agent_status,
            AgentStatus::Unknown,
            "an offline peer must never surface a stale idle/done"
        );
        assert_eq!(merged[0].last_known_status, Some(AgentStatus::Idle));
        assert_eq!(merged[0].reachability, Some(Reachability::Unreachable));
        assert_eq!(merged[0].name.as_deref(), Some("home/idler"));

        // A later success recovers immediately.
        assert_eq!(tracker.record_success(), Reachability::Reachable);
    }

    /// A peer with no `endpoint` is inbound-only: reconcile spawns no thread, adds
    /// no proxy route, and writes nothing to the store — the byte-identical
    /// no-outbound-federation path.
    #[test]
    fn reconcile_ignores_inbound_only_peer() {
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );
        manager.reconcile(&[FederationPeer {
            alias: "listen-only".into(),
            display_label: None,
            endpoint: None,
            profile_id: None,
            remote_session: None,
            token_file: None,
            expected_node_id: None,
            capability: CapabilityTier::Observe,
        }]);
        assert!(
            manager.live_aliases().is_empty(),
            "an endpoint-less peer must spawn no thread"
        );
        assert!(
            manager.registry_snapshot().is_empty(),
            "an endpoint-less peer must not be routable for proxying"
        );
        assert!(cache.lock().expect("cache lock").is_empty());
        manager.join_all();
    }

    #[cfg(unix)]
    #[test]
    fn ssh_federation_poll_and_proxy_share_named_remote_api_bridge() {
        use std::ffi::OsString;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct EnvRestore(Vec<(&'static str, Option<OsString>)>);
        impl Drop for EnvRestore {
            fn drop(&mut self) {
                for (key, value) in self.0.drain(..).rev() {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }

        let _env_lock = crate::config::test_config_env_lock().lock().unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "herdr-fed-remote-api-{}-{nonce}",
            std::process::id()
        ));
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let args_log = base.join("ssh-args");
        let fail_bridge = base.join("fail-bridge");
        let bridge_failed = base.join("bridge-failed");
        let ssh = bin.join("ssh");
        std::fs::write(
            &ssh,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HERDR_TEST_SSH_ARGS\"\nprintf '\\nherdr-remote-output-ready:1\\n'\nwhile IFS= read -r line; do\n  if [ -e \"$HERDR_TEST_SSH_FAIL\" ]; then\n    : > \"$HERDR_TEST_SSH_FAILED\"\n    printf '%s\\n' 'bridge-failed' >&2\n    exit 1\n  fi\n  case \"$line\" in\n    *'\"id\":\"api-client:status\"'*) printf '%s\\n' '{\"id\":\"api-client:status\",\"result\":{\"type\":\"pong\",\"version\":\"test\",\"protocol\":22}}' ;;\n    *) printf '%s\\n' \"$line\" ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ssh).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&ssh, permissions).unwrap();

        let _restore = EnvRestore(vec![
            ("PATH", std::env::var_os("PATH")),
            ("XDG_STATE_HOME", std::env::var_os("XDG_STATE_HOME")),
            (
                "HERDR_TEST_SSH_ARGS",
                std::env::var_os("HERDR_TEST_SSH_ARGS"),
            ),
            (
                "HERDR_TEST_SSH_FAIL",
                std::env::var_os("HERDR_TEST_SSH_FAIL"),
            ),
            (
                "HERDR_TEST_SSH_FAILED",
                std::env::var_os("HERDR_TEST_SSH_FAILED"),
            ),
        ]);
        std::env::set_var("PATH", &bin);
        std::env::set_var("XDG_STATE_HOME", base.join("state"));
        std::env::set_var("HERDR_TEST_SSH_ARGS", &args_log);
        std::env::set_var("HERDR_TEST_SSH_FAIL", &fail_bridge);
        std::env::set_var("HERDR_TEST_SSH_FAILED", &bridge_failed);

        let mut catalog = crate::client::endpoint::EndpointCatalog::default();
        let profile_id = catalog
            .add_ssh("Build", "dev@build.example", "agent-work")
            .unwrap();
        catalog.store_profiles().unwrap();
        let profile_text = profile_id.to_string();
        crate::client::endpoint::SshMetadataCache::new(
            &profile_text,
            "dev@build.example",
            "agent-work",
        )
        .unwrap()
        .store(&crate::client::endpoint::SshMachineMetadata {
            os: "linux".into(),
            executable: "/opt/Herdr Builds/herdr".into(),
        });

        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );
        let mut config = FederationConfig {
            coordinator: true,
            ..FederationConfig::default()
        };
        config.saved_machines.insert(
            profile_id.clone(),
            crate::config::FederationSavedMachinePolicy {
                expected_machine_id: "machine-build".into(),
                agent_grants: Vec::new(),
            },
        );
        manager.reconcile_config_for_session(&config, None);

        let route = manager
            .registry_snapshot()
            .get(&profile_text)
            .cloned()
            .expect("SSH peer is routable");
        let ConnectionTarget::SocketPath(socket) = route.target() else {
            panic!("SSH federation must route through the local saved bridge");
        };
        let stream = crate::ipc::connect_local_stream(socket).expect("connect shared bridge");
        let mut stream = BufReader::new(stream);
        stream.get_mut().write_all(b"probe\n").unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line, "probe\n");
        drop(stream);

        std::fs::write(&fail_bridge, b"fail").unwrap();
        let mut failing =
            crate::ipc::connect_local_stream(socket).expect("connect bridge before failure");
        failing.write_all(b"trigger\n").unwrap();
        drop(failing);
        let failure_deadline = Instant::now() + Duration::from_secs(2);
        while !bridge_failed.exists() && Instant::now() < failure_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(bridge_failed.exists(), "fake SSH process did not fail");
        std::fs::remove_file(&fail_bridge).unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let recovered_line = loop {
            if let Ok(stream) = crate::ipc::connect_local_stream(socket) {
                let mut recovered = BufReader::new(stream);
                if recovered.get_mut().write_all(b"recovered\n").is_ok() {
                    let mut line = String::new();
                    if recovered.read_line(&mut line).is_ok() && line == "recovered\n" {
                        break line;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "bridge supervisor did not recover after the SSH process failed"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(recovered_line, "recovered\n");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !args_log.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // The remote script travels base64-encoded (`echo <b64> | base64 --decode | sh`)
        // so login shells cannot reinterpret it; read the bridge command from the payload.
        let decoded_scripts = || {
            std::fs::read_to_string(&args_log)
                .expect("fake SSH invocation captured")
                .lines()
                .filter_map(|line| {
                    Some(
                        line.split("echo ")
                            .nth(1)?
                            .split_whitespace()
                            .next()?
                            .to_owned(),
                    )
                })
                .filter_map(|encoded| {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .ok()
                })
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .collect::<String>()
        };
        let args = std::fs::read_to_string(&args_log).expect("fake SSH invocation captured");
        let script = decoded_scripts();
        assert!(script.contains("'/opt/Herdr Builds/herdr'"), "{script}");
        assert!(script.contains("agent-work"), "{script}");
        assert!(script.contains("remote-api-bridge"), "{script}");
        assert!(args.contains("ControlMaster=auto"));
        assert!(args.contains("ControlPersist=600"));

        catalog.rename_ssh(&profile_id, "Build Farm").unwrap();
        catalog.store_profiles().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while manager
            .presentation_for(&profile_text)
            .is_some_and(|presentation| presentation.label != "Build Farm")
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            manager
                .presentation_for(&profile_text)
                .map(|presentation| presentation.label),
            Some("Build Farm".into())
        );
        assert_eq!(
            manager.reap_and_count_pending(),
            0,
            "catalog label rename must not reconnect federation"
        );

        crate::client::endpoint::SshMetadataCache::new(
            &profile_text,
            "dev@build.example",
            "agent-next",
        )
        .unwrap()
        .store(&crate::client::endpoint::SshMachineMetadata {
            os: "linux".into(),
            executable: "/opt/Herdr Builds/herdr".into(),
        });
        catalog.ssh[0].session = "agent-next".into();
        catalog.store_profiles().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while manager.remote_session_for(&profile_text).as_deref() != Some("agent-next")
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            manager.remote_session_for(&profile_text).as_deref(),
            Some("agent-next")
        );

        let route = manager
            .registry_snapshot()
            .get(&profile_text)
            .cloned()
            .expect("changed SSH session is routable");
        let ConnectionTarget::SocketPath(socket) = route.target() else {
            panic!("changed SSH federation route must remain a local bridge");
        };
        let stream = crate::ipc::connect_local_stream(socket).expect("connect replacement bridge");
        let mut stream = BufReader::new(stream);
        stream.get_mut().write_all(b"next\n").unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line, "next\n");
        drop(stream);
        let script = decoded_scripts();
        assert!(script.contains("agent-next"), "{script}");

        catalog.set_enabled(&profile_id, false);
        catalog.store_profiles().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while (!manager.live_aliases().is_empty()
            || manager.registry_snapshot().contains_key(&profile_text))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(manager.live_aliases().is_empty());
        assert!(manager.registry_snapshot().get(&profile_text).is_none());

        running.store(false, Ordering::Relaxed);
        manager.join_all();
        let _ = std::fs::remove_dir_all(base);
    }

    /// `reconcile(&[])` from empty is byte-identical to no federation: zero
    /// threads, an empty registry, an empty store, and no merged agents. And a
    /// full teardown (peers → none) leaves the same clean state.
    #[test]
    fn reconcile_empty_is_byte_identical() {
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );

        // From empty.
        manager.reconcile(&[]);
        assert!(manager.live_aliases().is_empty());
        assert!(manager.registry_snapshot().is_empty());
        assert!(cache.lock().expect("cache lock").is_empty());
        assert!(cache.lock().expect("cache lock").merged_agents().is_empty());

        // Full teardown: bring a peer up, then reconcile back to empty.
        let peer_srv = SeededPeer::spawn("builder");
        manager.reconcile(&[reachable_peer_on(
            peer_srv.addr,
            "home",
            &peer_srv.token_path,
        )]);
        assert!(wait_for_cached(&cache, "home", "home/builder"));
        manager.reconcile(&[]);
        assert!(
            manager.live_aliases().is_empty(),
            "teardown stops all threads"
        );
        assert!(
            manager.registry_snapshot().is_empty(),
            "teardown empties registry"
        );
        assert!(
            cache.lock().expect("cache lock").is_empty(),
            "teardown evicts every peer from the store"
        );
        assert!(cache.lock().expect("cache lock").merged_agents().is_empty());

        manager.join_all();
        peer_srv.shutdown();
    }

    /// Adding a second peer on a later reconcile spawns its thread and caches its
    /// agents without disturbing the first peer.
    #[test]
    fn reconcile_adds_second_peer_both_cached() {
        let peer_a = SeededPeer::spawn("a-agent");
        let peer_b = SeededPeer::spawn("b-agent");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );

        // First reconcile: only A.
        manager.reconcile(&[reachable_peer_on(peer_a.addr, "A", &peer_a.token_path)]);
        assert!(wait_for_cached(&cache, "A", "A/a-agent"));
        assert_eq!(manager.live_aliases(), vec!["A".to_string()]);

        // Second reconcile: A (unchanged) and B (added).
        manager.reconcile(&[
            reachable_peer_on(peer_a.addr, "A", &peer_a.token_path),
            reachable_peer_on(peer_b.addr, "B", &peer_b.token_path),
        ]);
        assert!(wait_for_cached(&cache, "B", "B/b-agent"));
        assert_eq!(
            manager.live_aliases(),
            vec!["A".to_string(), "B".to_string()]
        );
        {
            // A stayed cached across the add.
            let store = cache.lock().expect("cache lock");
            assert_eq!(
                store.peer("A").expect("A still cached").agents[0]
                    .name
                    .as_deref(),
                Some("A/a-agent")
            );
        }
        let registry = manager.registry_snapshot();
        assert!(registry.contains_key("A") && registry.contains_key("B"));

        manager.join_all();
        peer_a.shutdown();
        peer_b.shutdown();
    }

    /// Removing a peer on reconcile evicts it from the registry and the store
    /// immediately, stops its thread (which reaches `is_finished()` within a
    /// bounded wait, without block-joining on the test's critical path), and
    /// leaves the other peer cached.
    #[test]
    fn reconcile_removes_peer_stops_thread_and_evicts_store() {
        let peer_a = SeededPeer::spawn("a-agent");
        let peer_b = SeededPeer::spawn("b-agent");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );

        manager.reconcile(&[
            reachable_peer_on(peer_a.addr, "A", &peer_a.token_path),
            reachable_peer_on(peer_b.addr, "B", &peer_b.token_path),
        ]);
        assert!(wait_for_cached(&cache, "A", "A/a-agent"));
        assert!(wait_for_cached(&cache, "B", "B/b-agent"));

        // Reconcile to just B: A is removed.
        manager.reconcile(&[reachable_peer_on(peer_b.addr, "B", &peer_b.token_path)]);

        // A is evicted synchronously from the store and the registry; B remains.
        assert!(
            cache.lock().expect("cache lock").peer("A").is_none(),
            "removed peer A must be evicted from the store immediately"
        );
        assert!(cache.lock().expect("cache lock").peer("B").is_some());
        let registry = manager.registry_snapshot();
        assert!(
            !registry.contains_key("A"),
            "A must leave the proxy registry"
        );
        assert!(registry.contains_key("B"));
        assert_eq!(manager.live_aliases(), vec!["B".to_string()]);

        // A's retired thread reaches `is_finished()` within a bounded wait; the
        // reaper joins it only once finished, never blocking this path.
        let mut reaped = false;
        for _ in 0..300 {
            if manager.reap_and_count_pending() == 0 {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            reaped,
            "removed peer A's poll thread did not finish within the bounded wait"
        );

        manager.join_all();
        peer_a.shutdown();
        peer_b.shutdown();
    }

    /// Re-pointing an alias at a new endpoint retires the old thread and spawns a
    /// new one: the cache converges to the NEW addr's agent and never flaps back
    /// to a stale entry from the retiring old thread (exercises the
    /// under-store-lock stop guard), and the registry points at the new addr.
    #[test]
    fn reconcile_changed_endpoint_respawns() {
        let peer_old = SeededPeer::spawn("old-agent");
        let peer_new = SeededPeer::spawn("new-agent");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );

        // Alias A points at the OLD addr.
        manager.reconcile(&[reachable_peer_on(peer_old.addr, "A", &peer_old.token_path)]);
        assert!(wait_for_cached(&cache, "A", "A/old-agent"));
        let old_route = manager.registry_snapshot().get("A").cloned().unwrap();
        let old_stamp = old_route.stamp();

        // Re-point alias A at the NEW addr: endpoint changed → respawn.
        manager.reconcile(&[reachable_peer_on(peer_new.addr, "A", &peer_new.token_path)]);
        assert_eq!(manager.live_aliases(), vec!["A".to_string()]);
        assert!(
            !old_route.is_current(&old_stamp),
            "the retired transport generation must fence delayed replies"
        );
        let new_stamp = manager.registry_snapshot().get("A").unwrap().stamp();
        assert!(new_stamp.generation > old_stamp.generation);

        // The registry now routes A at the new addr.
        match manager.registry_snapshot().get("A").map(PeerRoute::target) {
            Some(ConnectionTarget::Tcp { addr, .. }) => assert_eq!(*addr, peer_new.addr),
            other => panic!("expected A to route to the new tcp addr, got {other:?}"),
        }

        // The cache converges to the NEW addr's agent.
        assert!(
            wait_for_cached(&cache, "A", "A/new-agent"),
            "cache did not converge to the re-pointed endpoint's agent"
        );

        // And it STAYS the new agent — the retiring old thread's under-lock stop
        // guard means it can never overwrite the evicted+respawned alias with a
        // stale "A/old-agent" entry.
        for _ in 0..20 {
            {
                let store = cache.lock().expect("cache lock");
                let entry = store.peer("A").expect("A cached");
                assert_eq!(
                    entry.agents[0].name.as_deref(),
                    Some("A/new-agent"),
                    "a retiring thread must never write a stale entry after eviction"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        manager.join_all();
        peer_old.shutdown();
        peer_new.shutdown();
    }

    #[test]
    fn reconcile_changed_identity_pin_respawns_and_revalidates_peer() {
        let peer_srv = SeededPeer::spawn("builder");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );

        let mut trusted = reachable_peer_on(peer_srv.addr, "A", &peer_srv.token_path);
        trusted.expected_node_id = Some("machine-peer".into());
        manager.reconcile(&[trusted.clone()]);
        assert!(wait_for_cached(&cache, "A", "A/builder"));

        let mut rotated = trusted.clone();
        rotated.expected_node_id = Some("machine-after-reinstall".into());
        manager.reconcile(&[rotated]);
        assert!(
            cache.lock().expect("cache lock").peer("A").is_none(),
            "changing the pin must synchronously evict state validated under the old pin"
        );

        manager.reconcile(&[trusted]);
        assert!(
            wait_for_cached(&cache, "A", "A/builder"),
            "restoring the matching pin must respawn and revalidate the peer"
        );

        manager.join_all();
        peer_srv.shutdown();
    }

    /// The under-store-lock stop guard skips the DEGRADE (miss) write once the
    /// peer's stop flag is set: the miss is still counted, but no cache entry is
    /// created for the evicted alias.
    #[test]
    fn poll_once_skips_degrade_write_when_peer_stop_set() {
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let dead_addr = {
            let probe = TcpListener::bind("127.0.0.1:0").expect("probe bind");
            probe.local_addr().expect("probe addr")
        };
        let client = ApiClient::for_target(ConnectionTarget::Tcp {
            addr: dead_addr,
            token: Some("t".into()),
        });
        let running = Arc::new(AtomicBool::new(true));
        // The peer is already retiring: the guard must skip the write.
        let peer_stop = Arc::new(AtomicBool::new(true));
        let mut tracker = ReachabilityTracker::default();

        let reachability = poll_once_into_cache(
            &client,
            "gone",
            None,
            None,
            &shared_presentation("gone"),
            &cache,
            &mut tracker,
            &running,
            &peer_stop,
        );
        assert_eq!(
            reachability,
            Reachability::Degraded,
            "the miss is still counted even though the write is skipped"
        );
        assert!(
            cache.lock().expect("cache lock").peer("gone").is_none(),
            "a stopped peer's degrade write must be skipped, leaving no entry"
        );
        assert!(cache.lock().expect("cache lock").is_empty());
    }

    /// The under-store-lock stop guard also skips the SUCCESS (set) write once the
    /// peer's stop flag is set: the poll succeeds, but nothing is cached for the
    /// evicted alias.
    #[test]
    fn poll_once_skips_set_write_when_peer_stop_set() {
        let peer_srv = SeededPeer::spawn("builder");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let client = ApiClient::for_target(ConnectionTarget::Tcp {
            addr: peer_srv.addr,
            token: Some(SEEDED_PEER_TOKEN.into()),
        });
        let running = Arc::new(AtomicBool::new(true));
        let peer_stop = Arc::new(AtomicBool::new(true));
        let mut tracker = ReachabilityTracker::default();

        let reachability = poll_once_into_cache(
            &client,
            "home",
            None,
            None,
            &shared_presentation("home"),
            &cache,
            &mut tracker,
            &running,
            &peer_stop,
        );
        assert_eq!(
            reachability,
            Reachability::Reachable,
            "the poll itself still succeeds"
        );
        assert!(
            cache.lock().expect("cache lock").peer("home").is_none(),
            "a stopped peer's successful write must be skipped, leaving no entry"
        );

        peer_srv.shutdown();
    }

    /// The home owns routing and reachability fields. A malicious/faulty peer
    /// cannot smuggle `machine_id`/`reachability`/`last_known_status` into the
    /// cache: `prefix_remote_agent` normalizes `machine_id` to the local alias and
    /// clears the poll-derived fields. The poll has already stamped the validated
    /// response-level `origin_machine_id`.
    #[test]
    fn prefix_remote_agent_discards_peer_supplied_federation_fields() {
        let hostile: AgentInfo = serde_json::from_value(serde_json::json!({
            "terminal_id": "term-remote",
            "name": "sneaky",
            "agent_status": "idle",
            "workspace_id": "ws-remote",
            "tab_id": "tab-remote",
            "pane_id": "pane-remote",
            "focused": false,
            "revision": 1,
            // Fields a peer must NOT be trusted to set:
            "machine_id": "not-home",
            "reachability": "reachable",
            "last_known_status": "working",
            "origin_machine_id": "machine-peer",
        }))
        .expect("hostile agent deserializes");
        // Sanity: the peer really did set the home-owned fields.
        assert_eq!(hostile.machine_id.as_deref(), Some("not-home"));
        assert_eq!(hostile.reachability, Some(Reachability::Reachable));
        assert_eq!(hostile.last_known_status, Some(AgentStatus::Working));

        let normalized = prefix_remote_agent("home", &presentation("home"), hostile)
            .expect("unqualified remote agent is accepted");
        assert_eq!(
            normalized.machine_id.as_deref(),
            Some("home"),
            "machine_id must be overwritten with the peer's local alias"
        );
        assert_eq!(
            normalized.reachability, None,
            "a peer-supplied reachability must be discarded"
        );
        assert_eq!(
            normalized.last_known_status, None,
            "a peer-supplied last_known_status must be discarded"
        );
        assert_eq!(normalized.name.as_deref(), Some("home/sneaky"));
        assert_eq!(normalized.machine_profile_id, None);
        assert_eq!(normalized.machine_label.as_deref(), Some("home"));
        assert_eq!(
            normalized.origin_machine_id.as_deref(),
            Some("machine-peer")
        );
    }

    #[test]
    fn prefix_remote_agent_rejects_already_qualified_identity() {
        let mut already_qualified = seeded_agent(AgentStatus::Working, "builder");
        already_qualified.terminal_id = "other/terminal".into();
        assert!(prefix_remote_agent("home", &presentation("home"), already_qualified).is_none());
    }

    #[test]
    fn prefix_remote_agent_allows_slashes_in_display_name() {
        let named = seeded_agent(AgentStatus::Working, "team/builder");
        let qualified = prefix_remote_agent("home", &presentation("home"), named)
            .expect("display names are not route ids");
        assert_eq!(qualified.name.as_deref(), Some("home/team/builder"));
    }

    #[test]
    fn prefix_remote_agent_preserves_archived_agent_with_empty_live_ids() {
        let mut archived = seeded_agent(AgentStatus::Idle, "archived");
        archived.workspace_id.clear();
        archived.tab_id.clear();
        archived.pane_id.clear();
        archived.archived = Some(crate::api::schema::AgentArchivedInfo {
            at: "2026-09-19T00:00:00Z".into(),
            by: "owner".into(),
            reason: None,
        });

        let qualified = prefix_remote_agent("home", &presentation("home"), archived)
            .expect("archived agent remains visible");
        assert_eq!(qualified.terminal_id, "home/term-remote");
        assert_eq!(qualified.workspace_id, "");
        assert_eq!(qualified.tab_id, "");
        assert_eq!(qualified.pane_id, "");
        assert!(qualified.archived.is_some());
    }

    /// FIX 1 (DoS hardening): a peer that returns an over-cap response line must
    /// not drive unbounded allocation. The poll's bounded read errors at ~the cap,
    /// the peer is degraded (a miss) rather than caching any agents, and clearing
    /// `running` then joins the poll thread promptly — an oversized/malicious
    /// response can neither OOM the home nor hang shutdown.
    #[test]
    fn federation_client_bounds_oversized_peer_response_and_shuts_down_promptly() {
        // A raw "peer" that accepts the poll connection, ignores the hello and the
        // request, and floods a single line far larger than the response cap with
        // NO newline. The bounded reader must reject it near the cap.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind hostile peer");
        let addr = listener.local_addr().expect("hostile peer addr");
        let peer_running = Arc::new(AtomicBool::new(true));
        let peer_flag = Arc::clone(&peer_running);
        let peer_thread = std::thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("hostile listener nonblocking");
            while peer_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        // One oversized, newline-free blob. Ignore write errors:
                        // the client closes the moment it crosses the cap.
                        let blob = vec![b'x'; FEDERATION_MAX_RESPONSE_BYTES + 8192];
                        let _ = sock.write_all(&blob);
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        let token_path = unique_token_file("hostile-token");
        let peer = reachable_peer_on(addr, "home", &token_path);
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let running = Arc::new(AtomicBool::new(true));
        let manager = FederationPeerManager::new(
            Arc::clone(&cache),
            EventHub::default(),
            Arc::clone(&running),
        );
        manager.reconcile(&[peer]);
        assert_eq!(manager.live_aliases(), vec!["home".to_string()]);

        // The over-cap response is rejected, so the peer is recorded as a miss
        // (Degraded on the first failure) rather than caching any agents.
        let mut degraded = false;
        for _ in 0..300 {
            {
                let store = cache.lock().expect("cache lock");
                if let Some(entry) = store.peer("home") {
                    if entry.reachability == Reachability::Degraded && entry.agents.is_empty() {
                        degraded = true;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            degraded,
            "an oversized peer response must degrade the peer, not cache agents"
        );

        // Shutdown must join the poll thread promptly (the bounded read can't
        // hang). Mirror `ServerHandle::drop`: clear `running` — which aborts any
        // in-flight bounded read — then `join_all`.
        let (join_tx, join_rx) = std::sync::mpsc::channel();
        let manager_for_join = Arc::clone(&manager);
        let running_for_join = Arc::clone(&running);
        std::thread::spawn(move || {
            running_for_join.store(false, Ordering::Relaxed);
            manager_for_join.join_all();
            let _ = join_tx.send(());
        });
        join_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("poll thread did not join promptly after shutdown");

        peer_running.store(false, Ordering::Relaxed);
        let _ = peer_thread.join();
        let _ = std::fs::remove_file(&token_path);
    }

    // ---- Outbound proxy router (W4) ----

    #[test]
    fn federated_split_routes_only_configured_alias_prefixes() {
        let mut peers = HashMap::new();
        peers.insert(
            "w1".to_string(),
            PeerRoute::for_test(ConnectionTarget::Tcp {
                addr: "127.0.0.1:9000".parse().unwrap(),
                token: None,
            }),
        );

        // `<alias>/x` routes; the rest keeps everything after the FIRST slash.
        assert_eq!(
            federated_split("w1/builder", &peers),
            Some(("w1", "builder"))
        );
        assert_eq!(federated_split("w1/ws/tab", &peers), Some(("w1", "ws/tab")));

        // A `/`-containing name whose first segment is NOT a configured alias.
        assert_eq!(federated_split("other/x", &peers), None);
        // A `w1:p1`-style local id (no slash) never routes, even though `w1` is an
        // alias — the colon is not the federation separator.
        assert_eq!(federated_split("w1:p1", &peers), None);
        // A bare local name and the exact alias without a slash never route.
        assert_eq!(federated_split("builder", &peers), None);
        assert_eq!(federated_split("w1", &peers), None);

        // With no configured peers nothing routes.
        assert_eq!(
            federated_split("w1/builder", &HashMap::<String, ()>::new()),
            None
        );
    }

    /// The tier table and the routing table MUST agree.
    ///
    /// `federation.rs` says which methods a peer may invoke; `routable_target_mut` says
    /// which ones actually get sent there. #181: `agent.rename` and `agent.focus` were
    /// AllowedAt(Admin) but had no routing arm, so they dispatched LOCALLY and answered
    /// `agent_not_found` for an `<alias>/pane` id that only exists on the peer - while
    /// `agent.read` on the same string worked, because reads were routed.
    ///
    /// This asserts the property rather than the two reported methods, so the next
    /// method added to one table and forgotten in the other fails here.
    #[test]
    fn every_federation_allowed_target_method_is_routable() {
        // (wire name, a params object carrying a federated target)
        let cases: &[(&str, serde_json::Value)] = &[
            (
                "agent.rename",
                serde_json::json!({ "target": "remote/w1:p1", "name": "x" }),
            ),
            (
                "agent.focus",
                serde_json::json!({ "target": "remote/w1:p1" }),
            ),
            (
                "agent.restart",
                serde_json::json!({ "target": "remote/w1:p1" }),
            ),
            (
                "pane.rename",
                serde_json::json!({ "pane_id": "remote/w1:p1", "label": "x" }),
            ),
            (
                "pane.input.set",
                serde_json::json!({ "pane_id": "remote/w1:p1", "right_click": "pane" }),
            ),
            (
                "pane.send_keys",
                serde_json::json!({ "pane_id": "remote/w1:p1", "keys": ["enter"] }),
            ),
            (
                "agent.answer",
                serde_json::json!({ "target": "remote/w1:p1", "ask_id": "call_1", "option": "A" }),
            ),
            (
                "pane.send_text",
                serde_json::json!({ "pane_id": "remote/w1:p1", "text": "hi" }),
            ),
            (
                "pane.history",
                serde_json::json!({ "pane_id": "remote/w1:p1", "cursor": "tok" }),
            ),
            (
                "workspace.get",
                serde_json::json!({ "workspace_id": "remote/w1:p1" }),
            ),
            ("tab.get", serde_json::json!({ "tab_id": "remote/w1:p1" })),
            (
                "tab.list",
                serde_json::json!({ "workspace_id": "remote/w1:p1" }),
            ),
        ];

        for (name, params) in cases {
            // Only assert routing for methods federation actually exposes to a peer.
            let access = crate::api::federation::federation_access(name);
            assert!(
                !matches!(access, crate::api::federation::FederationAccess::Denied),
                "{name} is Denied by federation; this case is stale"
            );

            let mut method: Method =
                serde_json::from_value(serde_json::json!({ "method": name, "params": params }))
                    .unwrap_or_else(|err| panic!("{name} params do not parse: {err}"));
            assert_eq!(
                routable_target_mut(&mut method).map(|t| t.as_str()),
                Some("remote/w1:p1"),
                "{name} is federation-allowed but is not routed to the peer, so an \
                 <alias>/pane target resolves locally and fails (#181)"
            );
        }
    }

    #[test]
    fn routable_target_mut_selects_the_proxyable_target_field() {
        // Agent methods carry the target in `target`; a rewrite is reflected.
        let mut prompt = Method::AgentPrompt(AgentPromptParams {
            target: "remote/a".into(),
            text: "hi".into(),
            wait: None,
        });
        assert_eq!(
            routable_target_mut(&mut prompt).map(|t| t.as_str()),
            Some("remote/a")
        );
        if let Some(target) = routable_target_mut(&mut prompt) {
            *target = "a".into();
        }
        assert!(matches!(&prompt, Method::AgentPrompt(p) if p.target == "a"));

        // Pane read/turns carry it in `pane_id` (W3 alias-prefixes that field too).
        let mut turns: Method = serde_json::from_value(serde_json::json!({
            "method": "pane.turns",
            "params": { "pane_id": "remote/p" },
        }))
        .unwrap();
        assert_eq!(
            routable_target_mut(&mut turns).map(|t| t.as_str()),
            Some("remote/p")
        );

        // Pane send-text/send-input carry it in `pane_id` too — the write path the
        // app uses for a federated pane's keystrokes (S-Tab / ^C / Ctrl-chords).
        let mut send_text: Method = serde_json::from_value(serde_json::json!({
            "method": "pane.send_text",
            "params": { "pane_id": "remote/p", "text": "\u{1b}[Z" },
        }))
        .unwrap();
        assert_eq!(
            routable_target_mut(&mut send_text).map(|t| t.as_str()),
            Some("remote/p")
        );
        let mut send_input: Method = serde_json::from_value(serde_json::json!({
            "method": "pane.send_input",
            "params": { "pane_id": "remote/p", "keys": ["c-c"] },
        }))
        .unwrap();
        assert_eq!(
            routable_target_mut(&mut send_input).map(|t| t.as_str()),
            Some("remote/p")
        );

        // pane.set_pty_size carries the target in `pane_id` too, so a federated
        // width-lease request routes to the owning peer (#137).
        let mut set_pty_size: Method = serde_json::from_value(serde_json::json!({
            "method": "pane.set_pty_size",
            "params": { "pane_id": "remote/p", "cols": 100, "rows": 40, "lock": true },
        }))
        .unwrap();
        assert_eq!(
            routable_target_mut(&mut set_pty_size).map(|t| t.as_str()),
            Some("remote/p")
        );

        // A current-pane (no `pane_id`) set_pty_size has no proxyable target and
        // stays local.
        let mut local_set_pty_size: Method = serde_json::from_value(serde_json::json!({
            "method": "pane.set_pty_size",
            "params": { "cols": 100, "rows": 40, "lock": true },
        }))
        .unwrap();
        assert!(routable_target_mut(&mut local_set_pty_size).is_none());

        // A method with no proxyable target is never routed.
        let mut list = Method::AgentList(crate::api::schema::AgentListParams::default());
        assert!(routable_target_mut(&mut list).is_none());
    }

    #[test]
    fn proxy_response_classifies_transport_failures_by_phase() {
        let running = Arc::new(AtomicBool::new(true));
        let request = Request {
            id: "u1".into(),
            method: Method::Ping(PingParams::default()),
        };

        // Connect/write failure (the request never left the home) → the request
        // was NOT delivered → `peer_unreachable`.
        let dead_addr = {
            let probe = TcpListener::bind("127.0.0.1:0").expect("probe bind");
            probe.local_addr().expect("probe addr")
            // `probe` drops here, freeing the port so connects are refused.
        };
        let unreachable = PeerRoute::for_test(ConnectionTarget::Tcp {
            addr: dead_addr,
            token: Some("t".into()),
        });
        let stamp = unreachable.stamp();
        let line = proxy_federated_response(&unreachable, &stamp, &request, &running);
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], "u1");
        assert_eq!(value["error"]["code"], "peer_unreachable");

        // Read failure AFTER a successful write (peer drops without answering) →
        // the peer may or may not have acted → `delivery_unknown`.
        let peer = start_proxy_peer(|_req, sock| {
            // A READ-phase failure (peer read the request, then never answered), made deterministic
            // across platforms — macOS included (refs #103). Half-close ONLY the response direction
            // so the home reads a clean EOF, and keep draining the request side so the peer never
            // RSTs the home's already-delivered write. A full `drop(sock)` here is a platform-timing
            // gamble: on macOS the close can RST an in-flight / lazily-flushed home write, which the
            // home then misclassifies as a write-phase `peer_unreachable` instead of the intended
            // read-phase `delivery_unknown`. Keeping the read side open (until the home closes or the
            // harness 2s read-timeout fires) guarantees the write lands and only the response EOFs.
            use std::io::Read as _;
            let _ = sock.shutdown(std::net::Shutdown::Write);
            let mut drain = [0u8; 256];
            loop {
                match (&sock).read(&mut drain) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let dropper = PeerRoute::for_test(ConnectionTarget::Tcp {
            addr: peer.addr,
            token: Some("t".into()),
        });
        let stamp = dropper.stamp();
        let line = proxy_federated_response(&dropper, &stamp, &request, &running);
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], "u1");
        assert_eq!(value["error"]["code"], "delivery_unknown");
    }

    #[test]
    fn delayed_response_from_previous_remote_boot_is_rejected() {
        use std::io::BufRead as _;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind peer");
        let addr = listener.local_addr().unwrap();
        let (request_seen_tx, request_seen_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept peer");
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            request_seen_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let response = serde_json::to_string(&SuccessResponse {
                id: "stale-1".into(),
                result: ResponseResult::Pong {
                    version: "test".into(),
                    protocol: 1,
                    capabilities: None,
                },
            })
            .unwrap();
            writeln!(socket, "{response}").unwrap();
        });

        let route = PeerRoute::for_test(ConnectionTarget::Tcp {
            addr,
            token: Some("t".into()),
        });
        route.force_remote_boot_for_test("boot-before");
        let request = Request {
            id: "stale-1".into(),
            method: Method::Ping(PingParams::default()),
        };
        let request_route = route.clone();
        let request_stamp = route.stamp();
        let running = Arc::new(AtomicBool::new(true));
        let request_running = Arc::clone(&running);
        let request_thread = std::thread::spawn(move || {
            proxy_federated_response(&request_route, &request_stamp, &request, &request_running)
        });
        request_seen_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("peer received request");
        route.force_remote_boot_for_test("boot-after");
        release_tx.send(()).unwrap();

        let line = request_thread.join().unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], "stale-1");
        assert_eq!(value["error"]["code"], "stale_peer_generation");
        peer.join().unwrap();
    }

    /// A raw loopback "peer daemon" for the outbound-proxy tests. It accepts
    /// connections in a loop (so a spurious home reconnect/retry would be
    /// observed), and for each: reads and discards the `federation.hello` line,
    /// reads one request line, records it in `seen`, then hands `(request_line,
    /// socket)` to `serve` to produce the peer's behavior — write a response line
    /// or drop the socket. Runs until `running` clears.
    struct ProxyPeer {
        addr: SocketAddr,
        seen: Arc<Mutex<Vec<String>>>,
        running: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for ProxyPeer {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn start_proxy_peer(serve: impl Fn(&str, TcpStream) + Send + 'static) -> ProxyPeer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy peer");
        listener
            .set_nonblocking(true)
            .expect("proxy peer nonblocking");
        let addr = listener.local_addr().expect("proxy peer addr");
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let running = Arc::new(AtomicBool::new(true));
        let seen_thread = Arc::clone(&seen);
        let running_thread = Arc::clone(&running);
        let thread = std::thread::spawn(move || {
            while running_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((sock, _)) => {
                        // The listener is non-blocking (for the accept loop). On macOS/BSD an
                        // ACCEPTED socket INHERITS the listener's non-blocking flag (Linux does
                        // not), which makes the blocking `read_line`s below return WouldBlock →
                        // the peer never reads the request, and the home then sees the connection
                        // break as a write-phase failure (peer_unreachable) instead of the intended
                        // read-phase delivery_unknown. Force the per-connection socket blocking so
                        // the peer behaves identically on every platform. refs #103
                        let _ = sock.set_nonblocking(false);
                        let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut reader =
                            BufReader::new(sock.try_clone().expect("clone proxy peer sock"));
                        let mut hello = String::new();
                        if reader.read_line(&mut hello).unwrap_or(0) == 0 {
                            continue;
                        }
                        let mut request = String::new();
                        if reader.read_line(&mut request).unwrap_or(0) == 0 {
                            continue;
                        }
                        seen_thread
                            .lock()
                            .expect("seen lock")
                            .push(request.trim().to_string());
                        serve(request.trim(), sock);
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        ProxyPeer {
            addr,
            seen,
            running,
            thread: Some(thread),
        }
    }

    /// A home daemon connection handler running over a loopback TCP pair standing
    /// in for a LOCAL client, with `registry` as its outbound proxy registry. The
    /// test writes requests to `client` and reads responses; `api_rx` lets a test
    /// prove whether a request reached the LOCAL app dispatch path.
    struct HomeConn {
        client: TcpStream,
        api_rx: mpsc::UnboundedReceiver<ApiRequestMessage>,
        running: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for HomeConn {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn drive_home(registry: HashMap<String, ConnectionTarget>) -> HomeConn {
        drive_home_routes(
            registry
                .into_iter()
                .map(|(alias, target)| (alias, PeerRoute::for_test(target)))
                .collect(),
        )
    }

    fn drive_home_routes(registry: HashMap<String, PeerRoute>) -> HomeConn {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind home listener");
        let addr = listener.local_addr().expect("home addr");
        let client = TcpStream::connect(addr).expect("connect home");
        let (server, _) = listener.accept().expect("accept home");
        let (api_tx, api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let event_hub = EventHub::default();
        let running = Arc::new(AtomicBool::new(true));
        let handler_running = Arc::clone(&running);
        let registry = Arc::new(registry);
        let thread = std::thread::spawn(move || {
            // A local client carries `federation: None` (never capability-gated).
            let _ = handle_connection_with_stop(
                ApiStream::Tcp(server),
                &api_tx,
                &event_hub,
                &handler_running,
                None,
                None,
                None,
                &registry,
                #[cfg(unix)]
                None,
            );
        });
        HomeConn {
            client,
            api_rx,
            running,
            thread: Some(thread),
        }
    }

    fn home_roundtrip(conn: &mut HomeConn, request: serde_json::Value) -> String {
        let encoded = serde_json::to_string(&request).expect("encode home request");
        writeln!(conn.client, "{encoded}").expect("write home request");
        conn.client.flush().expect("flush home request");
        let mut response = String::new();
        BufReader::new(conn.client.try_clone().expect("clone home client"))
            .read_line(&mut response)
            .expect("read home response");
        response.trim().to_string()
    }

    #[test]
    fn proxy_returns_the_peers_agent_prompted_verbatim() {
        // The peer answers agent.prompt for its LOCAL agent id (prefix stripped)
        // with a real AgentPrompted{delivery}. The exact line it sends is captured
        // so the home's reply can be asserted byte-for-byte identical.
        let expected = serde_json::to_string(&SuccessResponse {
            id: "p1".into(),
            result: ResponseResult::AgentPrompted {
                agent: seeded_agent(AgentStatus::Working, "builder"),
                delivery: Some(AgentPromptDelivery::Submitted),
            },
        })
        .unwrap();
        let peer_line = expected.clone();
        let peer = start_proxy_peer(move |request, mut sock| {
            let parsed: serde_json::Value =
                serde_json::from_str(request).expect("peer request is json");
            // The home stripped the `<alias>/` prefix before forwarding, and left
            // the id untouched so the peer can echo it.
            assert_eq!(parsed["method"], "agent.prompt");
            assert_eq!(parsed["params"]["target"], "builder");
            assert_eq!(parsed["id"], "p1");
            writeln!(sock, "{peer_line}").expect("peer writes response");
            let _ = sock.flush();
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "p1",
                "method": "agent.prompt",
                "params": { "target": "remote/builder", "text": "ship it" },
            }),
        );

        assert_eq!(
            response, expected,
            "the peer's response must pass through verbatim"
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["type"], "agent_prompted");
        assert_eq!(value["result"]["delivery"], "submitted");

        // A proxied request never touches the local app, and reaches the peer once.
        assert!(
            home.api_rx.try_recv().is_err(),
            "a proxied prompt reached the local app dispatch path"
        );
        assert_eq!(peer.seen.lock().expect("seen lock").len(), 1);
    }

    #[test]
    fn proxy_rejects_an_identity_pinned_route_before_validation() {
        let registry = HashMap::from([(
            "remote".to_string(),
            PeerRoute::for_test_unvalidated(ConnectionTarget::Tcp {
                addr: "127.0.0.1:9".parse().unwrap(),
                token: None,
            }),
        )]);
        let mut home = drive_home_routes(registry);

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "identity-gate",
                "method": "agent.prompt",
                "params": { "target": "remote/builder", "text": "must not route" },
            }),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "federation_identity_unverified");
        assert!(
            home.api_rx.try_recv().is_err(),
            "an unvalidated federated target reached the local app"
        );
    }

    #[test]
    fn retired_connection_snapshot_sends_no_unary_or_stream_request_bytes() {
        let peer = start_proxy_peer(|_, _| panic!("retired route reached its former peer"));
        let route = PeerRoute::for_test(ConnectionTarget::Tcp {
            addr: peer.addr,
            token: Some("tok".into()),
        });
        let registry = HashMap::from([("remote".to_string(), route.clone())]);
        route.retire();

        for request in [
            serde_json::json!({
                "id": "retired-unary",
                "method": "agent.prompt",
                "params": { "target": "remote/builder", "text": "must not send" },
            }),
            serde_json::json!({
                "id": "retired-stream",
                "method": "pane.stream",
                "params": { "pane_id": "remote/pane" },
            }),
        ] {
            let mut home = drive_home_routes(registry.clone());
            let response = home_roundtrip(&mut home, request);
            let value: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(value["error"]["code"], "stale_peer_generation");
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            peer.seen.lock().expect("seen lock").is_empty(),
            "a retired route delivered request bytes from an older connection snapshot"
        );
    }

    #[test]
    fn pane_stream_rejects_an_identity_pinned_route_before_validation() {
        let registry = HashMap::from([(
            "remote".to_string(),
            PeerRoute::for_test_unvalidated(ConnectionTarget::Tcp {
                addr: "127.0.0.1:9".parse().unwrap(),
                token: None,
            }),
        )]);
        let mut home = drive_home_routes(registry);

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "stream-identity-gate",
                "method": "pane.stream",
                "params": { "pane_id": "remote/pane" },
            }),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "federation_identity_unverified");
        assert!(
            home.api_rx.try_recv().is_err(),
            "an unvalidated federated stream reached the local app"
        );
    }

    #[test]
    fn unpinned_route_remains_enabled_after_poll_failure() {
        let route = PeerRoute::for_test(ConnectionTarget::Tcp {
            addr: "127.0.0.1:9".parse().unwrap(),
            token: None,
        });
        route.set_identity_validated(false);
        assert!(route.identity_validated());
    }

    #[test]
    fn proxy_returns_the_peers_read_snapshot_verbatim() {
        // agent.read (an Observe-tier read) to `<alias>/…` returns the peer's
        // snapshot unchanged. The response body shape is opaque to the home — it
        // passes the line through — so a marker field is enough to prove it.
        let expected = serde_json::json!({
            "id": "r1",
            "result": { "type": "pane_read", "read": { "marker": "peer-snapshot" } },
        })
        .to_string();
        let peer_line = expected.clone();
        let peer = start_proxy_peer(move |request, mut sock| {
            let parsed: serde_json::Value =
                serde_json::from_str(request).expect("peer request is json");
            assert_eq!(parsed["method"], "agent.read");
            assert_eq!(parsed["params"]["target"], "screen");
            writeln!(sock, "{peer_line}").expect("peer writes snapshot");
            let _ = sock.flush();
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "r1",
                "method": "agent.read",
                "params": { "target": "remote/screen", "source": "visible" },
            }),
        );

        assert_eq!(
            response, expected,
            "the peer snapshot must pass through verbatim"
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["read"]["marker"], "peer-snapshot");
        assert!(home.api_rx.try_recv().is_err());
    }

    #[test]
    fn proxy_routes_agent_transcript_and_after_polls_to_the_owning_peer() {
        // Both the first page and an `after` poll go to the peer with the alias
        // stripped and every cursor param intact; the peer's page comes back
        // verbatim.
        let page = |id: &str| {
            serde_json::json!({
                "id": id,
                "result": {
                    "type": "agent_transcript", "agent": "builder", "pane_id": "w1:p9",
                    "session_id": "s", "entries": [], "before": null, "after": "peer-cursor",
                    "pending_ask": null, "working": null,
                },
            })
            .to_string()
        };
        let peer = start_proxy_peer(move |request, mut sock| {
            let parsed: serde_json::Value = serde_json::from_str(request).unwrap();
            assert_eq!(parsed["method"], "agent.transcript");
            assert_eq!(parsed["params"]["target"], "w1:p9");
            writeln!(sock, "{}", page(parsed["id"].as_str().unwrap())).unwrap();
            let _ = sock.flush();
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);
        let first = home_roundtrip(
            &mut home,
            serde_json::json!({"id": "t1", "method": "agent.transcript",
                "params": {"target": "remote/w1:p9", "limit": 5}}),
        );
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["result"]["after"], "peer-cursor");
        let mut home = drive_home(HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]));
        let poll = home_roundtrip(
            &mut home,
            serde_json::json!({"id": "t2", "method": "agent.transcript",
                "params": {"target": "remote/w1:p9", "after": "peer-cursor"}}),
        );
        assert_eq!(poll, page("t2"));
        let seen = peer.seen.lock().unwrap().clone();
        let forwarded: Vec<serde_json::Value> = seen
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(forwarded[0]["params"]["limit"], 5);
        assert_eq!(forwarded[1]["params"]["after"], "peer-cursor");
        assert!(home.api_rx.try_recv().is_err(), "nothing ran on the home");
    }

    #[test]
    fn a_peer_too_old_for_agent_transcript_answers_transcript_unsupported() {
        // An older peer cannot parse the method and says `invalid_request`.
        let peer = start_proxy_peer(move |request, mut sock| {
            let parsed: serde_json::Value = serde_json::from_str(request).unwrap();
            let reply = error_response_json(
                parsed["id"].as_str().unwrap().into(),
                "invalid_request",
                "invalid request: unknown variant `agent.transcript`".into(),
            );
            writeln!(sock, "{reply}").unwrap();
            let _ = sock.flush();
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);
        let response = home_roundtrip(
            &mut home,
            serde_json::json!({"id": "old", "method": "agent.transcript",
                "params": {"target": "remote/builder"}}),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], "old");
        assert_eq!(value["error"]["code"], "transcript_unsupported");

        // Other routed methods keep the peer's verdict untouched.
        let mut home = drive_home(HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]));
        let response = home_roundtrip(
            &mut home,
            serde_json::json!({"id": "r", "method": "agent.read",
                "params": {"target": "remote/builder", "source": "visible"}}),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "invalid_request");
    }

    #[test]
    fn an_observe_peer_may_read_transcripts() {
        // The inbound gate: an Observe-tier federation peer's agent.transcript
        // reaches the local app instead of being refused.
        let mut fed = start_federation(one_peer("obs-tok", CapabilityTier::Observe));
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: fed.addr,
                token: Some("obs-tok".into()),
            },
        )]);
        let mut home = drive_home(registry);
        let client = std::thread::spawn(move || {
            home_roundtrip(
                &mut home,
                serde_json::json!({"id": "o1", "method": "agent.transcript",
                    "params": {"target": "remote/agent"}}),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let message = loop {
            if let Ok(message) = fed.api_rx.try_recv() {
                break message;
            }
            assert!(Instant::now() < deadline, "the peer's app never saw it");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(matches!(
            &message.request.method,
            Method::AgentTranscript(params) if params.target == "agent"
        ));
        let _ = message.respond_to.send(error_response_json(
            message.request.id,
            "transcript_unavailable",
            "stub".into(),
        ));
        let response = client.join().unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "transcript_unavailable");
    }

    #[test]
    fn proxy_passes_through_a_peers_forbidden_verdict() {
        // A REAL Observe-tier federation listener stands in for the peer. agent.
        // prompt needs Interact, so the peer's OWN capability gate returns
        // `forbidden` before any dispatch — and the home passes it straight
        // through, applying no capability logic of its own.
        let mut fed = start_federation(one_peer("obs-tok", CapabilityTier::Observe));
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: fed.addr,
                token: Some("obs-tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "f1",
                "method": "agent.prompt",
                "params": { "target": "remote/agent", "text": "hi" },
            }),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], "f1");
        assert_eq!(value["error"]["code"], "forbidden");

        // The peer's gate rejected before dispatch: nothing reached its app, and
        // nothing reached the home's app either.
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            fed.api_rx.try_recv().is_err(),
            "a forbidden proxied call reached the peer's app"
        );
        assert!(home.api_rx.try_recv().is_err());
    }

    #[test]
    fn proxy_reports_delivery_unknown_and_never_retries_when_the_peer_drops() {
        // The peer reads the request then drops the connection WITHOUT a response.
        let peer = start_proxy_peer(|_request, sock| {
            drop(sock);
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "d1",
                "method": "agent.prompt",
                "params": { "target": "remote/builder", "text": "hi" },
            }),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], "d1");
        assert_eq!(value["error"]["code"], "delivery_unknown");

        // The peer saw exactly one request: a post-write read failure is NEVER
        // auto-retried (a write that did land must not be duplicated).
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            peer.seen.lock().expect("seen lock").len(),
            1,
            "the proxy retried after a post-write read failure"
        );
        assert!(home.api_rx.try_recv().is_err());
    }

    #[test]
    fn local_prompt_without_alias_prefix_is_not_routed() {
        // A peer that fails the test if it is ever contacted.
        let peer = start_proxy_peer(|_request, _sock| {
            panic!("a local prompt must never reach the peer");
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        // App responder: answer the LOCAL agent.prompt, proving it hit local
        // dispatch with an UNREWRITTEN target.
        let mut api_rx = std::mem::replace(&mut home.api_rx, mpsc::unbounded_channel().1);
        let responder = std::thread::spawn(move || {
            for _ in 0..300 {
                if let Ok(msg) = api_rx.try_recv() {
                    let target = match &msg.request.method {
                        Method::AgentPrompt(params) => params.target.clone(),
                        other => panic!("unexpected local method: {other:?}"),
                    };
                    assert_eq!(
                        target, "local-builder",
                        "a local target must not be rewritten"
                    );
                    let resp = serde_json::to_string(&SuccessResponse {
                        id: msg.request.id.clone(),
                        result: ResponseResult::AgentPrompted {
                            agent: seeded_agent(AgentStatus::Working, "local-builder"),
                            delivery: Some(AgentPromptDelivery::Submitted),
                        },
                    })
                    .unwrap();
                    let _ = msg.respond_to.send(resp);
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        });

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "loc1",
                "method": "agent.prompt",
                "params": { "target": "local-builder", "text": "hi" },
            }),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], "loc1");
        assert_eq!(value["result"]["type"], "agent_prompted");

        assert!(
            responder.join().unwrap(),
            "the local prompt never reached the app dispatch path"
        );
        assert!(
            peer.seen.lock().expect("seen lock").is_empty(),
            "a local prompt was proxied to the peer"
        );
    }

    // ---- W5: federated pane.stream proxy ------------------------------------

    /// Send a request to the home connection and return a buffered line reader over
    /// its client socket, for reading a MULTI-line streamed response.
    fn home_stream_reader(conn: &mut HomeConn, request: serde_json::Value) -> BufReader<TcpStream> {
        let encoded = serde_json::to_string(&request).expect("encode home request");
        writeln!(conn.client, "{encoded}").expect("write home request");
        conn.client.flush().expect("flush home request");
        BufReader::new(conn.client.try_clone().expect("clone home client"))
    }

    /// Read one NDJSON line (trailing newline stripped), or `None` at EOF — the home
    /// closes the client connection when the proxied stream ends.
    fn next_stream_line(reader: &mut BufReader<TcpStream>) -> Option<String> {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line.trim_end().to_string()),
            Err(err) => panic!("stream read error: {err}"),
        }
    }

    #[test]
    fn proxy_streams_the_peers_frames_reprefixing_stream_started() {
        // The peer serves a stream for its LOCAL pane id (prefix stripped): a
        // stream_started ack, two data frames, and an exited frame, then closes.
        let started = serde_json::to_string(&SuccessResponse {
            id: "s1".into(),
            result: ResponseResult::StreamStarted {
                pane_id: "screen".into(),
                epoch: 7,
                cols: 80,
                rows: 24,
                base_seq: 100,
                resync: true,
            },
        })
        .unwrap();
        // Opaque-to-the-home control/data frames, asserted byte-verbatim below.
        let data1 =
            r#"{"stream":"pane.bytes","frame":"data","seq":100,"epoch":7,"data_b64":"aGk="}"#;
        let data2 =
            r#"{"stream":"pane.bytes","frame":"data","seq":102,"epoch":7,"data_b64":"Ynll"}"#;
        let exited = r#"{"stream":"pane.bytes","frame":"exited","seq":104,"epoch":7}"#;

        let started_peer = started.clone();
        let peer = start_proxy_peer(move |request, mut sock| {
            let parsed: serde_json::Value =
                serde_json::from_str(request).expect("peer request is json");
            // The home stripped the `<alias>/` prefix and preserved the id.
            assert_eq!(parsed["method"], "pane.stream");
            assert_eq!(parsed["params"]["pane_id"], "screen");
            assert_eq!(parsed["id"], "s1");
            writeln!(sock, "{started_peer}").expect("peer writes stream_started");
            writeln!(sock, "{data1}").expect("peer writes data1");
            writeln!(sock, "{data2}").expect("peer writes data2");
            writeln!(sock, "{exited}").expect("peer writes exited");
            let _ = sock.flush();
            // Return → sock drops → the stream closes.
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        let mut reader = home_stream_reader(
            &mut home,
            serde_json::json!({
                "id": "s1",
                "method": "pane.stream",
                "params": { "pane_id": "remote/screen" },
            }),
        );

        // Line 1: stream_started, pane_id RE-PREFIXED to `<alias>/…`, id preserved,
        // geometry carried through.
        let l1 = next_stream_line(&mut reader).expect("stream_started line");
        let v1: serde_json::Value = serde_json::from_str(&l1).unwrap();
        assert_eq!(v1["id"], "s1");
        assert_eq!(v1["result"]["type"], "stream_started");
        assert_eq!(v1["result"]["pane_id"], "remote/screen");
        assert_eq!(v1["result"]["epoch"], 7);
        assert_eq!(v1["result"]["cols"], 80);
        assert_eq!(v1["result"]["rows"], 24);

        // Lines 2..4: the data/exited frames pass through BYTE-VERBATIM, in order.
        assert_eq!(next_stream_line(&mut reader).as_deref(), Some(data1));
        assert_eq!(next_stream_line(&mut reader).as_deref(), Some(data2));
        assert_eq!(next_stream_line(&mut reader).as_deref(), Some(exited));

        // Peer closed → the home ends the stream (client sees EOF).
        assert!(
            next_stream_line(&mut reader).is_none(),
            "the stream did not end when the peer closed"
        );

        // A proxied stream never touches the local app, and reaches the peer once.
        assert!(
            home.api_rx.try_recv().is_err(),
            "a proxied pane.stream reached the local app dispatch path"
        );
        assert_eq!(peer.seen.lock().expect("seen lock").len(), 1);
    }

    #[test]
    fn pane_stream_stops_before_forwarding_a_stale_generation_frame() {
        let started = serde_json::to_string(&SuccessResponse {
            id: "stale-stream".into(),
            result: ResponseResult::StreamStarted {
                pane_id: "screen".into(),
                epoch: 7,
                cols: 80,
                rows: 24,
                base_seq: 100,
                resync: true,
            },
        })
        .unwrap();
        let stale_data =
            r#"{"stream":"pane.bytes","frame":"data","seq":100,"epoch":7,"data_b64":"c3RhbGU="}"#;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stream peer");
        let addr = listener.local_addr().unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept stream peer");
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            writeln!(socket, "{started}").unwrap();
            socket.flush().unwrap();
            release_rx.recv().unwrap();
            writeln!(socket, "{stale_data}").unwrap();
            socket.flush().unwrap();
        });

        let route = PeerRoute::for_test(ConnectionTarget::Tcp {
            addr,
            token: Some("tok".into()),
        });
        route.force_remote_boot_for_test("boot-before");
        let mut home = drive_home_routes(HashMap::from([("remote".to_string(), route.clone())]));
        let mut reader = home_stream_reader(
            &mut home,
            serde_json::json!({
                "id": "stale-stream",
                "method": "pane.stream",
                "params": { "pane_id": "remote/screen" },
            }),
        );
        let started = next_stream_line(&mut reader).expect("stream_started line");
        let value: serde_json::Value = serde_json::from_str(&started).unwrap();
        assert_eq!(value["result"]["type"], "stream_started");

        route.force_remote_boot_for_test("boot-after");
        release_tx.send(()).unwrap();
        let stale = next_stream_line(&mut reader).expect("stale generation error");
        let value: serde_json::Value = serde_json::from_str(&stale).unwrap();
        assert_eq!(value["error"]["code"], "stale_peer_generation");
        assert!(
            next_stream_line(&mut reader).is_none(),
            "stale peer frame must not cross the generation fence"
        );
        peer.join().unwrap();
    }

    #[test]
    fn proxy_passes_through_a_denied_pane_stream_verbatim() {
        // An observe-denied peer answers pane.stream with a `forbidden` first line;
        // the home forwards it verbatim (its allowlist is authoritative) and ends.
        let forbidden =
            r#"{"id":"s2","error":{"code":"forbidden","message":"pane.stream not permitted"}}"#;
        let peer = start_proxy_peer(move |request, mut sock| {
            let parsed: serde_json::Value =
                serde_json::from_str(request).expect("peer request is json");
            assert_eq!(parsed["method"], "pane.stream");
            assert_eq!(parsed["params"]["pane_id"], "screen");
            writeln!(sock, "{forbidden}").expect("peer writes forbidden");
            let _ = sock.flush();
            // Keep the connection open until the home has consumed the response and closes, so the
            // peer's close never RSTs the home BEFORE it reads the buffered `forbidden` line. A bare
            // drop here races the home's read on macOS — the RST arrives first and the home reports
            // a broken-pipe `peer_unreachable` instead of forwarding the verdict. refs #103
            use std::io::Read as _;
            let mut drain = [0u8; 256];
            loop {
                match (&sock).read(&mut drain) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        let mut reader = home_stream_reader(
            &mut home,
            serde_json::json!({
                "id": "s2",
                "method": "pane.stream",
                "params": { "pane_id": "remote/screen" },
            }),
        );

        let l1 = next_stream_line(&mut reader).expect("forbidden line");
        assert_eq!(l1, forbidden, "a peer error must pass through verbatim");
        let v1: serde_json::Value = serde_json::from_str(&l1).unwrap();
        assert_eq!(v1["error"]["code"], "forbidden");

        // No stream frames follow a forbidden verdict; the stream ends.
        assert!(
            next_stream_line(&mut reader).is_none(),
            "the stream did not end after the forbidden line"
        );
        assert!(home.api_rx.try_recv().is_err());
    }

    #[test]
    fn proxy_closes_an_over_cap_streamed_frame() {
        // After a valid stream_started, the peer floods a single frame far larger
        // than the per-frame cap with NO newline. The proxy must close the stream
        // (bounded — no unbounded allocation); the giant frame is never delivered.
        let started = serde_json::to_string(&SuccessResponse {
            id: "s3".into(),
            result: ResponseResult::StreamStarted {
                pane_id: "screen".into(),
                epoch: 1,
                cols: 80,
                rows: 24,
                base_seq: 0,
                resync: true,
            },
        })
        .unwrap();
        let started_peer = started.clone();
        let peer = start_proxy_peer(move |_request, mut sock| {
            writeln!(sock, "{started_peer}").expect("peer writes stream_started");
            let _ = sock.flush();
            // One oversized, newline-free blob. Ignore write errors: the home closes
            // the moment the in-progress frame crosses the cap.
            let blob = vec![b'x'; FEDERATION_MAX_STREAM_FRAME_BYTES + 8192];
            let _ = sock.write_all(&blob);
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        let mut reader = home_stream_reader(
            &mut home,
            serde_json::json!({
                "id": "s3",
                "method": "pane.stream",
                "params": { "pane_id": "remote/screen" },
            }),
        );

        // The stream_started ack arrives (re-prefixed) ...
        let l1 = next_stream_line(&mut reader).expect("stream_started line");
        let v1: serde_json::Value = serde_json::from_str(&l1).unwrap();
        assert_eq!(v1["result"]["type"], "stream_started");
        assert_eq!(v1["result"]["pane_id"], "remote/screen");

        // ... then the over-cap frame closes the stream with nothing more delivered.
        assert!(
            next_stream_line(&mut reader).is_none(),
            "the over-cap frame was forwarded instead of closing the stream"
        );
    }

    #[test]
    fn local_pane_stream_without_alias_prefix_is_served_locally() {
        // A peer that fails the test if it is ever contacted.
        let peer = start_proxy_peer(|_request, _sock| {
            panic!("a local pane.stream must never reach the peer");
        });
        let registry = HashMap::from([(
            "remote".to_string(),
            ConnectionTarget::Tcp {
                addr: peer.addr,
                token: Some("tok".into()),
            },
        )]);
        let mut home = drive_home(registry);

        // App responder: proves the request reached LOCAL dispatch via
        // `pane_output_stream::serve`, whose first act is a PaneStreamOpen carrying
        // the UNREWRITTEN pane id. Answer the open with an error (so serve returns
        // fast) and the follow-up close with ok, and report the pane id it saw.
        let mut api_rx = std::mem::replace(&mut home.api_rx, mpsc::unbounded_channel().1);
        let responder = std::thread::spawn(move || {
            let mut seen_open: Option<String> = None;
            for _ in 0..300 {
                if let Ok(msg) = api_rx.try_recv() {
                    match &msg.request.method {
                        Method::PaneStreamOpen(params) => {
                            seen_open = Some(params.pane_id.clone());
                            let resp = error_response_json(
                                msg.request.id.clone(),
                                "pane_not_found",
                                "no such pane".into(),
                            );
                            let _ = msg.respond_to.send(resp);
                        }
                        Method::PaneStreamClose(_) => {
                            let resp = serde_json::to_string(&SuccessResponse {
                                id: msg.request.id.clone(),
                                result: ResponseResult::Ok {},
                            })
                            .unwrap();
                            let _ = msg.respond_to.send(resp);
                            return seen_open;
                        }
                        other => panic!("unexpected local method: {other:?}"),
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            seen_open
        });

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "loc-stream",
                "method": "pane.stream",
                "params": { "pane_id": "local-pane" },
            }),
        );
        // serve wrote the pane_not_found error (the open failed) to the client.
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], "loc-stream");
        assert_eq!(value["error"]["code"], "pane_not_found");

        let seen = responder.join().unwrap();
        assert_eq!(
            seen.as_deref(),
            Some("local-pane"),
            "the local pane.stream did not reach pane_output_stream::serve with an unrewritten id"
        );
        assert!(
            peer.seen.lock().expect("seen lock").is_empty(),
            "a local pane.stream was proxied to the peer"
        );
    }

    // ---- #243: federated workspaces ------------------------------------------

    fn build_box_presentation() -> Arc<RwLock<PeerPresentation>> {
        Arc::new(RwLock::new(PeerPresentation {
            profile_id: Some("profile-1".into()),
            label: "Build Box".into(),
        }))
    }

    fn peer_tab(tab_id: &str, workspace_id: &str, number: usize) -> crate::api::schema::TabInfo {
        serde_json::from_value(serde_json::json!({
            "tab_id": tab_id,
            "workspace_id": workspace_id,
            "number": number,
            "label": format!("tab-{number}"),
            "focused": number == 1,
            "pane_count": 1,
            "agent_status": "idle",
        }))
        .expect("peer tab deserializes")
    }

    fn seeded_peer_client(peer: &SeededPeer) -> ApiClient {
        ApiClient::for_target(ConnectionTarget::Tcp {
            addr: peer.addr,
            token: Some(SEEDED_PEER_TOKEN.into()),
        })
    }

    #[test]
    fn qualify_remote_workspace_and_tab_prefix_every_id_field() {
        let presentation = build_box_presentation().read().unwrap().clone();
        let mut workspace = peer_workspace("w1", 3, "api", AgentStatus::Working);
        // Peer-supplied federation fields are home-owned and must be replaced.
        workspace.machine_id = Some("spoofed".into());
        workspace.reachability = Some(Reachability::Reachable);
        workspace.last_known_status = Some(AgentStatus::Idle);
        let workspace =
            qualify_remote_workspace("home", &presentation, workspace).expect("qualifies");
        assert_eq!(workspace.workspace_id, "home/w1");
        assert_eq!(workspace.active_tab_id, "home/w1:t1");
        assert_eq!((workspace.number, workspace.label.as_str()), (3, "api"));
        assert_eq!(workspace.agent_status, AgentStatus::Working);
        assert_eq!(workspace.machine_id.as_deref(), Some("home"));
        assert_eq!(workspace.machine_label.as_deref(), Some("Build Box"));
        assert_eq!(workspace.machine_profile_id.as_deref(), Some("profile-1"));
        assert_eq!(workspace.reachability, None);
        assert_eq!(workspace.last_known_status, None);

        let tab = qualify_remote_tab("home", &presentation, peer_tab("w1:t2", "w1", 2))
            .expect("qualifies");
        assert_eq!(tab.tab_id, "home/w1:t2");
        assert_eq!(tab.workspace_id, "home/w1");
        assert_eq!((tab.number, tab.label.as_str()), (2, "tab-2"));
        assert_eq!(tab.machine_id.as_deref(), Some("home"));
        assert_eq!(tab.machine_label.as_deref(), Some("Build Box"));
        assert_eq!(tab.machine_profile_id.as_deref(), Some("profile-1"));

        // Already-qualified ids are refused, never prefixed twice.
        assert!(qualify_remote_workspace(
            "home",
            &presentation,
            peer_workspace("x/w1", 1, "a", AgentStatus::Idle)
        )
        .is_none());
        let mut nested_tab = peer_workspace("w1", 1, "a", AgentStatus::Idle);
        nested_tab.active_tab_id = "x/w1:t1".into();
        assert!(qualify_remote_workspace("home", &presentation, nested_tab).is_none());
        assert!(qualify_remote_tab("home", &presentation, peer_tab("x/t", "w1", 1)).is_none());
        assert!(qualify_remote_tab("home", &presentation, peer_tab("t", "x/w1", 1)).is_none());
    }

    #[test]
    fn federation_poll_caches_peer_workspaces_with_their_own_number_label_and_order() {
        let peer_srv = SeededPeer::spawn("builder");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let client = seeded_peer_client(&peer_srv);
        let running = Arc::new(AtomicBool::new(true));
        let peer_stop = Arc::new(AtomicBool::new(false));
        let mut tracker = ReachabilityTracker::default();
        let mut refresh = WorkspaceRefresh::default();
        let presentation = build_box_presentation();

        assert_eq!(
            poll_once_into_cache(
                &client,
                "home",
                None,
                None,
                &presentation,
                &cache,
                &mut tracker,
                &running,
                &peer_stop,
            ),
            Reachability::Reachable
        );
        refresh_workspaces_into_cache(
            &client,
            "home",
            &presentation,
            &cache,
            &mut refresh,
            Instant::now(),
            &running,
            &peer_stop,
        );

        let merged = cache.lock().expect("cache lock").merged_workspaces();
        let summary: Vec<_> = merged
            .iter()
            .map(|ws| {
                (
                    ws.workspace_id.as_str(),
                    ws.number,
                    ws.label.as_str(),
                    ws.focused,
                    ws.active_tab_id.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("home/ws-remote", 1, "api", true, "home/ws-remote:t1"),
                ("home/ws-docs", 2, "docs", false, "home/ws-docs:t1"),
            ],
            "the peer's own workspaces, in its order; its re-exported one is dropped"
        );
        assert!(merged.iter().all(|ws| {
            ws.machine_id.as_deref() == Some("home")
                && ws.machine_label.as_deref() == Some("Build Box")
                && ws.reachability == Some(Reachability::Reachable)
        }));

        running.store(false, Ordering::Relaxed);
        peer_srv.shutdown();
    }

    #[test]
    fn federation_workspace_fetch_runs_at_the_slow_cadence_not_every_agent_poll() {
        let peer_srv = SeededPeer::spawn("builder");
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let client = seeded_peer_client(&peer_srv);
        let running = Arc::new(AtomicBool::new(true));
        let peer_stop = Arc::new(AtomicBool::new(false));
        let mut tracker = ReachabilityTracker::default();
        let mut refresh = WorkspaceRefresh::default();
        let shared = shared_presentation("home");
        let fetches = || peer_srv.workspace_list_requests.load(Ordering::SeqCst);
        let t0 = Instant::now();
        let refresh_at = |refresh: &mut WorkspaceRefresh, at: Duration| {
            refresh_workspaces_into_cache(
                &client,
                "home",
                &shared,
                &cache,
                refresh,
                t0 + at,
                &running,
                &peer_stop,
            );
        };

        // Six 5 s agent polls spanning 25 s fetch workspaces exactly once.
        for tick in 0..6u32 {
            assert_eq!(
                poll_once_into_cache(
                    &client,
                    "home",
                    None,
                    None,
                    &shared,
                    &cache,
                    &mut tracker,
                    &running,
                    &peer_stop,
                ),
                Reachability::Reachable
            );
            refresh_at(&mut refresh, FEDERATION_POLL_INTERVAL * tick);
        }
        assert_eq!(
            fetches(),
            1,
            "agent polls inside the slow interval must not refetch"
        );
        refresh_at(&mut refresh, FEDERATION_WORKSPACE_POLL_INTERVAL);
        assert_eq!(fetches(), 2, "the slow interval elapsed");

        // The peer creates a workspace and a polled agent names it: one early fetch.
        peer_srv.workspaces.lock().unwrap().push(peer_workspace(
            "ws-new",
            3,
            "new",
            AgentStatus::Idle,
        ));
        let mut agent = prefix_remote_agent(
            "home",
            &presentation("home"),
            seeded_agent(AgentStatus::Working, "builder"),
        )
        .unwrap();
        agent.workspace_id = "home/ws-new".into();
        cache
            .lock()
            .unwrap()
            .set_peer("home", PeerCacheEntry::reachable(vec![agent.clone()], t0));
        refresh_at(&mut refresh, Duration::from_secs(35));
        assert_eq!(
            fetches(),
            3,
            "an uncached workspace id refreshes immediately"
        );
        assert!(cache
            .lock()
            .unwrap()
            .merged_workspaces()
            .iter()
            .any(|ws| ws.workspace_id == "home/ws-new" && ws.number == 3));
        refresh_at(&mut refresh, Duration::from_secs(40));
        assert_eq!(fetches(), 3);

        // An id the peer never lists triggers one early fetch, not one per poll.
        agent.workspace_id = "home/ws-ghost".into();
        cache
            .lock()
            .unwrap()
            .set_peer("home", PeerCacheEntry::reachable(vec![agent], t0));
        refresh_at(&mut refresh, Duration::from_secs(45));
        assert_eq!(fetches(), 4);
        refresh_at(&mut refresh, Duration::from_secs(50));
        refresh_at(&mut refresh, Duration::from_secs(55));
        assert_eq!(
            fetches(),
            4,
            "a still-missing id must not refetch every agent poll"
        );
        refresh_at(&mut refresh, Duration::from_secs(75));
        assert_eq!(fetches(), 5);

        running.store(false, Ordering::Relaxed);
        peer_srv.shutdown();
    }

    #[test]
    fn unreachable_peer_keeps_cached_workspaces_marked_stale() {
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        {
            let mut store = cache.lock().expect("cache lock");
            store.set_peer(
                "home",
                PeerCacheEntry::reachable(Vec::new(), Instant::now()),
            );
            store.set_peer_workspaces(
                "home",
                vec![qualify_remote_workspace(
                    "home",
                    &presentation("home"),
                    peer_workspace("ws-remote", 4, "api", AgentStatus::Working),
                )
                .unwrap()],
            );
        }
        let dead_addr = {
            let probe = TcpListener::bind("127.0.0.1:0").expect("probe bind");
            probe.local_addr().expect("probe addr")
        };
        let client = ApiClient::for_target(ConnectionTarget::Tcp {
            addr: dead_addr,
            token: Some("t".into()),
        });
        let running = Arc::new(AtomicBool::new(true));
        let peer_stop = Arc::new(AtomicBool::new(false));
        let mut tracker = ReachabilityTracker::default();
        let mut last = Reachability::Reachable;
        for _ in 0..3 {
            last = poll_once_into_cache(
                &client,
                "home",
                None,
                None,
                &shared_presentation("home"),
                &cache,
                &mut tracker,
                &running,
                &peer_stop,
            );
        }
        assert_eq!(last, Reachability::Unreachable);

        let merged = cache.lock().expect("cache lock").merged_workspaces();
        assert_eq!(merged.len(), 1, "last-known workspaces survive the outage");
        let ws = &merged[0];
        assert_eq!(ws.workspace_id, "home/ws-remote");
        assert_eq!((ws.number, ws.label.as_str()), (4, "api"));
        assert_eq!(ws.reachability, Some(Reachability::Unreachable));
        assert_eq!(ws.agent_status, AgentStatus::Unknown);
        assert_eq!(ws.last_known_status, Some(AgentStatus::Working));
    }

    /// A proxy peer answering the routed workspace/tab reads with peer-local ids.
    fn start_workspace_proxy_peer() -> ProxyPeer {
        start_proxy_peer(|request, mut sock| {
            let parsed: serde_json::Value =
                serde_json::from_str(request).expect("peer request is json");
            let params = &parsed["params"];
            let result = match parsed["method"].as_str() {
                Some("workspace.get") if params["workspace_id"] == "bad" => {
                    ResponseResult::WorkspaceInfo {
                        workspace: peer_workspace("x/bad", 1, "bad", AgentStatus::Idle),
                    }
                }
                Some("workspace.get") => {
                    assert_eq!(params["workspace_id"], "w1", "alias prefix stripped");
                    ResponseResult::WorkspaceInfo {
                        workspace: peer_workspace("w1", 2, "api", AgentStatus::Working),
                    }
                }
                Some("tab.get") => {
                    assert_eq!(params["tab_id"], "w1:t1");
                    ResponseResult::TabInfo {
                        tab: peer_tab("w1:t1", "w1", 1),
                    }
                }
                Some("tab.list") => {
                    assert_eq!(params["workspace_id"], "w1");
                    ResponseResult::TabList {
                        tabs: vec![peer_tab("w1:t1", "w1", 1), peer_tab("w1:t2", "w1", 2)],
                    }
                }
                other => panic!("unexpected proxied method {other:?}"),
            };
            let line = serde_json::to_string(&SuccessResponse {
                id: parsed["id"].as_str().expect("request id").into(),
                result,
            })
            .unwrap();
            writeln!(sock, "{line}").expect("peer writes response");
            let _ = sock.flush();
        })
    }

    fn workspace_home(peer: &ProxyPeer) -> HomeConn {
        let route = PeerRoute::for_test(ConnectionTarget::Tcp {
            addr: peer.addr,
            token: Some("tok".into()),
        })
        .with_presentation(build_box_presentation());
        drive_home_routes(HashMap::from([("remote".to_string(), route)]))
    }

    #[test]
    fn workspace_and_tab_reads_route_to_the_peer_with_qualified_replies() {
        let peer = start_workspace_proxy_peer();
        let roundtrip = |request: serde_json::Value| -> serde_json::Value {
            let mut home = workspace_home(&peer);
            // A request that wrongly dispatches locally gets no reply here;
            // fail instead of hanging.
            home.client
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let response = home_roundtrip(&mut home, request);
            assert!(
                home.api_rx.try_recv().is_err(),
                "a federated workspace/tab read reached the local app"
            );
            serde_json::from_str(&response).unwrap()
        };

        let value = roundtrip(serde_json::json!({
            "id": "g1",
            "method": "workspace.get",
            "params": { "workspace_id": "remote/w1" },
        }));
        assert_eq!(value["id"], "g1");
        let workspace = &value["result"]["workspace"];
        assert_eq!(workspace["workspace_id"], "remote/w1");
        assert_eq!(workspace["active_tab_id"], "remote/w1:t1");
        assert_eq!(workspace["number"], 2);
        assert_eq!(workspace["label"], "api");
        assert_eq!(workspace["machine_id"], "remote");
        assert_eq!(workspace["machine_label"], "Build Box");
        assert_eq!(workspace["machine_profile_id"], "profile-1");

        let value = roundtrip(serde_json::json!({
            "id": "t1",
            "method": "tab.get",
            "params": { "tab_id": "remote/w1:t1" },
        }));
        assert_eq!(value["result"]["tab"]["tab_id"], "remote/w1:t1");
        assert_eq!(value["result"]["tab"]["workspace_id"], "remote/w1");
        assert_eq!(value["result"]["tab"]["machine_id"], "remote");

        let value = roundtrip(serde_json::json!({
            "id": "l1",
            "method": "tab.list",
            "params": { "workspace_id": "remote/w1" },
        }));
        let tabs: Vec<_> = value["result"]["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tab| {
                (
                    tab["tab_id"].clone(),
                    tab["workspace_id"].clone(),
                    tab["number"].clone(),
                )
            })
            .collect();
        assert_eq!(
            tabs,
            [
                ("remote/w1:t1".into(), "remote/w1".into(), 1.into()),
                ("remote/w1:t2".into(), "remote/w1".into(), 2.into()),
            ]
        );

        let value = roundtrip(serde_json::json!({
            "id": "b1",
            "method": "workspace.get",
            "params": { "workspace_id": "remote/bad" },
        }));
        assert_eq!(value["id"], "b1");
        assert_eq!(value["error"]["code"], "peer_invalid_response");

        assert_eq!(peer.seen.lock().unwrap().len(), 4);
    }

    #[test]
    fn workspace_get_with_an_unknown_alias_dispatches_locally_unrewritten() {
        let peer = start_proxy_peer(|_request, _sock| {
            panic!("an unknown alias must never reach a peer");
        });
        let mut home = workspace_home(&peer);
        let mut api_rx = std::mem::replace(&mut home.api_rx, mpsc::unbounded_channel().1);
        let responder = std::thread::spawn(move || {
            for _ in 0..300 {
                if let Ok(msg) = api_rx.try_recv() {
                    let Method::WorkspaceGet(target) = &msg.request.method else {
                        panic!("unexpected local method: {:?}", msg.request.method);
                    };
                    let target = target.workspace_id.clone();
                    let _ = msg.respond_to.send(error_response_json(
                        msg.request.id.clone(),
                        "workspace_not_found",
                        format!("workspace {target} not found"),
                    ));
                    return Some(target);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        });

        let response = home_roundtrip(
            &mut home,
            serde_json::json!({
                "id": "u1",
                "method": "workspace.get",
                "params": { "workspace_id": "nope/w1" },
            }),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "workspace_not_found");
        assert_eq!(
            responder.join().unwrap().as_deref(),
            Some("nope/w1"),
            "an unknown alias reaches local dispatch with its id untouched"
        );
        assert!(peer.seen.lock().unwrap().is_empty());
    }

    // ---- Peer event relay (#245) ----

    /// What a [`StreamingPeer`]'s app answers, shared across restarts of the
    /// peer at the same address.
    struct StreamingPeerState {
        agents: Mutex<Vec<AgentInfo>>,
        boot_id: Mutex<String>,
        events_v2: bool,
        /// Every `pane.list` id with its arrival: a subscribe probe
        /// (`…:sub:0:probe`) marks a new stream, the all-pane snapshot a live
        /// stream takes about once a second (`…:sub:0:panes`) marks a live one.
        pane_lists: Mutex<Vec<(Instant, String)>>,
    }

    fn streaming_state(
        agents: Vec<AgentInfo>,
        boot_id: &str,
        events_v2: bool,
    ) -> Arc<StreamingPeerState> {
        Arc::new(StreamingPeerState {
            agents: Mutex::new(agents),
            boot_id: Mutex::new(boot_id.into()),
            events_v2,
            pane_lists: Mutex::new(Vec::new()),
        })
    }

    /// A loopback federation peer with a real event hub behind the real
    /// `events.subscribe` stream, so a test pushes peer events and the relay
    /// reads them exactly as a daemon serves them.
    struct StreamingPeer {
        addr: SocketAddr,
        event_hub: EventHub,
        running: Arc<AtomicBool>,
        listener: JoinHandle<()>,
        responder: JoinHandle<()>,
        state: Arc<StreamingPeerState>,
    }

    impl StreamingPeer {
        fn spawn(state: Arc<StreamingPeerState>) -> Self {
            Self::spawn_on("127.0.0.1:0".parse().unwrap(), state)
        }

        fn spawn_on(addr: SocketAddr, state: Arc<StreamingPeerState>) -> Self {
            let listener = TcpListener::bind(addr).expect("bind streaming peer");
            let addr = listener.local_addr().expect("streaming peer addr");
            let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
            let event_hub = EventHub::default();
            let running = Arc::new(AtomicBool::new(true));
            let listener_thread = spawn_federation_listener(
                listener,
                one_peer(SEEDED_PEER_TOKEN, CapabilityTier::Observe),
                api_tx,
                event_hub.clone(),
                None,
                Arc::clone(&running),
                None,
            )
            .expect("spawn streaming peer listener");
            let app = Arc::clone(&state);
            let responder = std::thread::spawn(move || {
                while let Some(msg) = api_rx.blocking_recv() {
                    let id = msg.request.id;
                    let result = match msg.request.method {
                        Method::AgentList(params) => {
                            assert!(params.local_only, "the relay must poll local-only state");
                            ResponseResult::AgentList {
                                agents: app.agents.lock().unwrap().clone(),
                                origin_machine_id: Some("machine-peer".into()),
                                origin_boot_id: Some(app.boot_id.lock().unwrap().clone()),
                                origin_version: Some("test".into()),
                                origin_protocol: Some(crate::protocol::PROTOCOL_VERSION),
                                origin_capabilities: Some(ServerCapabilities {
                                    events_v2: app.events_v2,
                                    ..default_capabilities().unwrap()
                                }),
                            }
                        }
                        Method::PaneList(_) => {
                            app.pane_lists
                                .lock()
                                .unwrap()
                                .push((Instant::now(), id.clone()));
                            ResponseResult::PaneList { panes: Vec::new() }
                        }
                        Method::WorkspaceList(_) => ResponseResult::WorkspaceList {
                            workspaces: Vec::new(),
                        },
                        other => {
                            let _ = msg.respond_to.send(error_response_json(
                                id,
                                "unexpected_dispatch",
                                format!("streaming peer does not answer {other:?}"),
                            ));
                            continue;
                        }
                    };
                    let _ = msg
                        .respond_to
                        .send(serde_json::to_string(&SuccessResponse { id, result }).unwrap());
                }
            });
            Self {
                addr,
                event_hub,
                running,
                listener: listener_thread,
                responder,
                state,
            }
        }

        /// `events.subscribe` streams opened so far.
        fn subscribes(&self) -> usize {
            self.state
                .pane_lists
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, id)| id.ends_with(":sub:0:probe"))
                .count()
        }

        /// Streams that took an all-pane snapshot within `window`.
        fn live_streams(&self, window: Duration) -> HashSet<String> {
            let since = Instant::now() - window;
            self.state
                .pane_lists
                .lock()
                .unwrap()
                .iter()
                .filter(|(at, _)| *at >= since)
                .filter_map(|(_, id)| id.strip_suffix(":sub:0:panes").map(str::to_owned))
                .collect()
        }

        /// Stop the peer, dropping every stream, and return its address so a
        /// restarted peer can take it over.
        fn shutdown(self) -> SocketAddr {
            self.running.store(false, Ordering::Relaxed);
            let _ = self.listener.join();
            let _ = self.responder.join();
            self.addr
        }
    }

    fn peer_agent(pane_id: &str, status: AgentStatus) -> AgentInfo {
        serde_json::from_value(serde_json::json!({
            "terminal_id": format!("t-{pane_id}"),
            "name": pane_id,
            "agent_status": status,
            "workspace_id": "w1",
            "tab_id": "w1:t1",
            "pane_id": pane_id,
            "focused": false,
            "revision": 1,
        }))
        .expect("peer agent deserializes")
    }

    fn peer_status(pane_id: &str, agent_status: AgentStatus) -> EventEnvelope {
        EventEnvelope {
            event: EventKind::PaneAgentStatusChanged,
            data: EventData::PaneAgentStatusChanged {
                pane_id: pane_id.into(),
                workspace_id: "w1".into(),
                agent_status,
                input_pending: false,
                input_prompt_kind: None,
                agent: None,
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
                turn: None,
                turn_epoch: None,
            },
        }
    }

    fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    /// Status events in `hub` as `(pane_id, status)`, in hub order.
    fn hub_statuses(hub: &EventHub) -> Vec<(String, AgentStatus)> {
        hub.events_after(0)
            .into_iter()
            .filter_map(|(_, event)| match event.data {
                EventData::PaneAgentStatusChanged {
                    pane_id,
                    agent_status,
                    ..
                } => Some((pane_id, agent_status)),
                _ => None,
            })
            .collect()
    }

    fn status_pairs(pairs: &[(&str, AgentStatus)]) -> Vec<(String, AgentStatus)> {
        pairs
            .iter()
            .map(|(pane_id, status)| ((*pane_id).to_owned(), *status))
            .collect()
    }

    fn relay_baseline_has(store: &Mutex<FederationStore>, pane_id: &str) -> bool {
        store
            .lock()
            .unwrap()
            .peer("box")
            .is_some_and(|entry| entry.relayed.contains_key(pane_id))
    }

    /// One relay thread for alias `box` against `addr`, driven directly (no
    /// poll thread) with fast retries. Its store already holds the peer's
    /// poll observation advertising `events_v2`.
    struct RelayFixture {
        hub: EventHub,
        store: Arc<Mutex<FederationStore>>,
        route: PeerRoute,
        stop: Arc<AtomicBool>,
        join: JoinHandle<()>,
    }

    fn start_relay(addr: SocketAddr) -> RelayFixture {
        start_relay_on(PeerRoute::for_test(ConnectionTarget::Tcp {
            addr,
            token: Some(SEEDED_PEER_TOKEN.into()),
        }))
    }

    fn start_relay_on(route: PeerRoute) -> RelayFixture {
        let hub = EventHub::default();
        let store = Arc::new(Mutex::new(FederationStore::default()));
        store.lock().unwrap().set_peer(
            "box",
            PeerCacheEntry::reachable_observed(
                Vec::new(),
                Instant::now(),
                PeerObservation {
                    remote_capabilities: default_capabilities(),
                    ..PeerObservation::default()
                },
            ),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let relay = PeerRelay {
            alias: "box".into(),
            expected_node_id: None,
            route: route.clone(),
            presentation: shared_presentation("box"),
            store: Arc::clone(&store),
            event_hub: hub.clone(),
            running: Arc::new(AtomicBool::new(true)),
            stop: Arc::clone(&stop),
            timing: RelayTiming {
                eligibility_step: Duration::from_millis(20),
                reconnect_base: Duration::from_millis(50),
                read_slice: Duration::from_millis(50),
            },
        };
        RelayFixture {
            hub,
            store,
            route,
            stop,
            join: std::thread::spawn(move || relay.run()),
        }
    }

    impl RelayFixture {
        fn stop(self) {
            self.stop.store(true, Ordering::Relaxed);
            self.join.join().expect("relay thread exits");
        }
    }

    /// A coordinator's API surface over loopback federation TCP: the real
    /// subscription stream on `hub`, with an app that has no local panes and
    /// merges `cache` into `agent.list` like the real one.
    struct CoordinatorFixture {
        addr: SocketAddr,
        running: Arc<AtomicBool>,
        listener: JoinHandle<()>,
        responder: JoinHandle<()>,
    }

    fn start_coordinator(hub: EventHub, cache: Arc<Mutex<FederationStore>>) -> CoordinatorFixture {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind coordinator");
        let addr = listener.local_addr().expect("coordinator addr");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let running = Arc::new(AtomicBool::new(true));
        let listener = spawn_federation_listener(
            listener,
            one_peer(SEEDED_PEER_TOKEN, CapabilityTier::Observe),
            api_tx,
            hub,
            None,
            Arc::clone(&running),
            None,
        )
        .expect("spawn coordinator listener");
        let responder = std::thread::spawn(move || {
            while let Some(msg) = api_rx.blocking_recv() {
                let id = msg.request.id;
                let result = match msg.request.method {
                    Method::PaneList(_) => ResponseResult::PaneList { panes: Vec::new() },
                    Method::AgentList(_) => ResponseResult::AgentList {
                        agents: cache.lock().unwrap().merged_agents(),
                        origin_machine_id: None,
                        origin_boot_id: None,
                        origin_version: None,
                        origin_protocol: None,
                        origin_capabilities: None,
                    },
                    Method::PaneGet(params) => {
                        let _ = msg.respond_to.send(error_response_json(
                            id,
                            "pane_not_found",
                            format!("pane {} not found", params.pane_id),
                        ));
                        continue;
                    }
                    other => panic!("coordinator app got {other:?}"),
                };
                let _ = msg
                    .respond_to
                    .send(serde_json::to_string(&SuccessResponse { id, result }).unwrap());
            }
        });
        CoordinatorFixture {
            addr,
            running,
            listener,
            responder,
        }
    }

    impl CoordinatorFixture {
        /// Open `events.subscribe` with `params`; every line arrives on the
        /// returned channel.
        fn subscribe(
            &self,
            params: serde_json::Value,
        ) -> std::sync::mpsc::Receiver<serde_json::Value> {
            let request: Request = serde_json::from_value(serde_json::json!({
                "id": "coordinator_sub",
                "method": "events.subscribe",
                "params": params,
            }))
            .unwrap();
            let client = tcp_client(self.addr, Some(SEEDED_PEER_TOKEN));
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let lines = client.request_stream(&request).expect("open subscription");
                for line in lines {
                    let Ok(line) = line else { return };
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            });
            rx
        }

        fn shutdown(self) {
            self.running.store(false, Ordering::Relaxed);
            let _ = self.listener.join();
            let _ = self.responder.join();
        }
    }

    /// End to end through the peer manager: the poll discovers `events_v2`,
    /// the relay streams the peer, and a status change reaches a coordinator
    /// all-pane subscriber and an `<alias>/<pane>` subscriber qualified well
    /// within a second; the store follows the event.
    #[test]
    fn peer_status_reaches_coordinator_subscribers_qualified_within_a_second() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let token_path = unique_token_file(SEEDED_PEER_TOKEN);
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let hub = EventHub::default();
        let running = Arc::new(AtomicBool::new(true));
        let coordinator = start_coordinator(hub.clone(), Arc::clone(&cache));
        let manager =
            FederationPeerManager::new(Arc::clone(&cache), hub.clone(), Arc::clone(&running));
        manager.reconcile(&[reachable_peer_on(peer.addr, "box", &token_path)]);
        assert!(
            wait_until(Duration::from_secs(10), || peer.subscribes() == 1
                && relay_baseline_has(&cache, "box/w1:p1")),
            "the relay did not open and seed its stream"
        );

        let lines = coordinator.subscribe(serde_json::json!({
            "subscriptions": [
                {"type": "pane.agent_status_changed"},
                {"type": "pane.agent_status_changed", "pane_id": "box/w1:p1"},
                {"type": "pane.agent_status_changed", "pane_id": "box/w9:p9"},
                {"type": "pane.turn_completed", "pane_id": "box/w1:p1"},
            ],
            "events_v2": true,
        }));
        let ack = lines.recv_timeout(Duration::from_secs(5)).expect("ack");
        assert_eq!(ack["result"]["type"], "subscription_started");
        assert_eq!(
            ack["result"]["rejected"],
            serde_json::json!([{"index": 2, "error": {"code": "pane_not_found", "message": "pane box/w9:p9 not found"}}]),
            "an unknown remote pane is rejected, a cached one is accepted"
        );

        let pushed = Instant::now();
        peer.event_hub
            .push(peer_status("w1:p1", AgentStatus::Blocked));
        let all_panes = lines
            .recv_timeout(Duration::from_secs(2))
            .expect("relayed event");
        let latency = pushed.elapsed();
        let single_pane = lines
            .recv_timeout(Duration::from_secs(2))
            .expect("relayed event");
        assert!(
            latency < Duration::from_secs(1),
            "relay latency {latency:?}"
        );
        for line in [&all_panes, &single_pane] {
            assert_eq!(line["event"], "pane.agent_status_changed");
            assert_eq!(line["data"]["pane_id"], "box/w1:p1");
            assert_eq!(line["data"]["workspace_id"], "box/w1");
            assert_eq!(line["data"]["agent_status"], "blocked");
        }
        assert_eq!(
            cache.lock().unwrap().merged_agents()[0].agent_status,
            AgentStatus::Blocked,
            "the relayed event patches the cached agent before the next poll"
        );

        let pane: crate::api::schema::PaneInfo = serde_json::from_value(serde_json::json!({
            "pane_id": "w1:p1", "terminal_id": "t-w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
            "focused": false, "agent_status": "idle", "revision": 2,
        }))
        .unwrap();
        peer.event_hub.push(EventEnvelope {
            event: EventKind::PaneTurnCompleted,
            data: EventData::PaneTurnCompleted {
                pane,
                turn: 3,
                turn_epoch: 1,
                outcome: crate::terminal::TurnOutcome::Completed,
                message: None,
                message_truncated: false,
                agent_session_path: None,
                completed_unix_ms: 1,
            },
        });
        let turn = lines
            .recv_timeout(Duration::from_secs(2))
            .expect("relayed turn");
        assert_eq!(turn["event"], "pane.turn_completed");
        assert_eq!(turn["data"]["pane"]["pane_id"], "box/w1:p1");
        let agent = cache.lock().unwrap().merged_agents().remove(0);
        assert_eq!((agent.turn, agent.turn_epoch), (Some(3), Some(1)));

        manager.join_all();
        coordinator.shutdown();
        peer.shutdown();
        let _ = std::fs::remove_file(&token_path);
    }

    /// A peer that relays its own peers marks those events in its hub and
    /// leaves them out of the `local_only` stream a coordinator reads, so a
    /// third machine's events are never re-exported (even with ids the
    /// coordinator could otherwise qualify).
    #[test]
    fn relayed_events_of_a_coordinator_peer_are_not_reexported() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let relay = start_relay(peer.addr);
        assert!(wait_until(Duration::from_secs(5), || relay_baseline_has(
            &relay.store,
            "box/w1:p1"
        )));

        peer.event_hub
            .push_relayed(peer_status("w7:p7", AgentStatus::Idle));
        peer.event_hub
            .push(peer_status("w1:p1", AgentStatus::Blocked));
        assert!(wait_until(Duration::from_secs(5), || !hub_statuses(
            &relay.hub
        )
        .is_empty()));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            hub_statuses(&relay.hub),
            status_pairs(&[("box/w1:p1", AgentStatus::Blocked)])
        );

        relay.stop();
        peer.shutdown();
    }

    /// A lost connection and a `lagged` peer stream both resync from
    /// `agent.list`: each transition missed meanwhile is published once, and
    /// the stream repeating one of them publishes nothing more.
    #[test]
    fn relay_resyncs_after_reconnect_and_lag_without_duplicates() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let relay = start_relay(peer.addr);
        assert!(wait_until(Duration::from_secs(5), || relay_baseline_has(
            &relay.store,
            "box/w1:p1"
        )));
        assert!(
            relay.hub.events_after(0).is_empty(),
            "the first stream seeds silently"
        );
        peer.event_hub
            .push(peer_status("w1:p1", AgentStatus::Blocked));
        assert!(wait_until(Duration::from_secs(5), || hub_statuses(
            &relay.hub
        )
        .len()
            == 1));

        // The connection drops; meanwhile p1 goes idle and p2 appears.
        let addr = peer.shutdown();
        *state.agents.lock().unwrap() = vec![
            peer_agent("w1:p1", AgentStatus::Idle),
            peer_agent("w1:p2", AgentStatus::Working),
        ];
        let peer = StreamingPeer::spawn_on(addr, Arc::clone(&state));
        assert!(wait_until(Duration::from_secs(10), || hub_statuses(
            &relay.hub
        )
        .len()
            == 3));
        // The restored stream repeats p1's idle transition late.
        peer.event_hub.push(peer_status("w1:p1", AgentStatus::Idle));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(hub_statuses(&relay.hub).len(), 3);

        // p2 blocks with its event lost to a ring overrun on the peer.
        state.agents.lock().unwrap()[1] = peer_agent("w1:p2", AgentStatus::Blocked);
        for index in 0..3 * EventHub::MAX_EVENTS {
            peer.event_hub.push(EventEnvelope {
                event: EventKind::WorkspaceFocused,
                data: EventData::WorkspaceFocused {
                    workspace_id: format!("w{index}"),
                },
            });
        }
        assert!(wait_until(Duration::from_secs(5), || hub_statuses(
            &relay.hub
        )
        .len()
            == 4));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            hub_statuses(&relay.hub),
            status_pairs(&[
                ("box/w1:p1", AgentStatus::Blocked),
                ("box/w1:p1", AgentStatus::Idle),
                ("box/w1:p2", AgentStatus::Working),
                ("box/w1:p2", AgentStatus::Blocked),
            ])
        );

        relay.stop();
        peer.shutdown();
    }

    /// The relay hands a remote agent's release and pane exit to the app loop
    /// (qualified, in order), which tells an agent that died from one that
    /// finished; a plain agent detection is not a release.
    #[test]
    fn relay_queues_remote_release_and_exit_for_push() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let relay = start_relay(peer.addr);
        assert!(wait_until(Duration::from_secs(5), || relay_baseline_has(
            &relay.store,
            "box/w1:p1"
        )));
        let detected = |released| EventEnvelope {
            event: EventKind::PaneAgentDetected,
            data: EventData::PaneAgentDetected {
                pane_id: "w1:p1".into(),
                workspace_id: "w1".into(),
                agent: Some("claude".into()),
                released,
                final_status: None,
            },
        };
        peer.event_hub.push(detected(false));
        peer.event_hub.push(detected(true));
        peer.event_hub.push(EventEnvelope {
            event: EventKind::PaneExited,
            data: EventData::PaneExited {
                pane_id: "w1:p1".into(),
                workspace_id: "w1".into(),
            },
        });
        assert!(wait_until(Duration::from_secs(5), || relay
            .hub
            .events_after(0)
            .iter()
            .any(|(_, event)| event.event == EventKind::PaneExited)));
        assert_eq!(
            relay.store.lock().unwrap().take_pane_events(),
            [
                (
                    "box/w1:p1".to_owned(),
                    crate::api::federation_store::RemotePaneEvent::AgentReleased
                ),
                (
                    "box/w1:p1".to_owned(),
                    crate::api::federation_store::RemotePaneEvent::Exited
                ),
            ]
        );

        relay.stop();
        peer.shutdown();
    }

    /// A poll that stops vouching for a pinned peer's identity ends its live
    /// stream: events the peer sends afterwards are neither published nor
    /// recorded, and the relay streams again only once the identity is
    /// validated anew.
    #[test]
    fn relay_stops_relaying_once_peer_identity_is_revoked() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let route = PeerRoute::for_test_unvalidated(ConnectionTarget::Tcp {
            addr: peer.addr,
            token: Some(SEEDED_PEER_TOKEN.into()),
        });
        route.set_identity_validated(true);
        let relay = start_relay_on(route);
        assert!(wait_until(Duration::from_secs(5), || peer.subscribes()
            == 1
            && relay_baseline_has(&relay.store, "box/w1:p1")));
        peer.event_hub
            .push(peer_status("w1:p1", AgentStatus::Blocked));
        assert!(wait_until(Duration::from_secs(5), || hub_statuses(
            &relay.hub
        )
        .len()
            == 1));

        // The poll found a different machine behind the endpoint.
        relay.route.set_identity_validated(false);
        peer.event_hub.push(peer_status("w1:p1", AgentStatus::Idle));
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            hub_statuses(&relay.hub),
            status_pairs(&[("box/w1:p1", AgentStatus::Blocked)]),
            "a revoked peer's events must not be relayed"
        );
        assert_eq!(
            relay.store.lock().unwrap().peer("box").unwrap().relayed["box/w1:p1"]
                .event
                .agent_status,
            AgentStatus::Blocked,
            "a revoked peer's events must not be recorded"
        );
        assert_eq!(peer.subscribes(), 1);

        relay.route.set_identity_validated(true);
        assert!(
            wait_until(Duration::from_secs(5), || peer.subscribes() == 2),
            "the revoked stream ended, so a validated peer opens a new one"
        );

        relay.stop();
        peer.shutdown();
    }

    /// A poll that misses on transport says nothing about the identity behind
    /// the endpoint: a validated peer's live stream keeps relaying.
    #[test]
    fn relay_keeps_streaming_when_a_poll_misses_on_transport() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let route = PeerRoute::for_test_unvalidated(ConnectionTarget::Tcp {
            addr: peer.addr,
            token: Some(SEEDED_PEER_TOKEN.into()),
        });
        route.set_identity_validated(true);
        let relay = start_relay_on(route);
        assert!(wait_until(Duration::from_secs(5), || peer.subscribes()
            == 1
            && relay_baseline_has(&relay.store, "box/w1:p1")));

        // The next poll cannot connect at all.
        let dead_addr = {
            let probe = TcpListener::bind("127.0.0.1:0").expect("probe bind");
            probe.local_addr().expect("probe addr")
        };
        poll_once_into_cache(
            &ApiClient::for_target(ConnectionTarget::Tcp {
                addr: dead_addr,
                token: Some(SEEDED_PEER_TOKEN.into()),
            }),
            "box",
            Some("machine-peer"),
            Some(&relay.route),
            &shared_presentation("box"),
            &relay.store,
            &mut ReachabilityTracker::default(),
            &Arc::new(AtomicBool::new(true)),
            &Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(
            relay
                .store
                .lock()
                .unwrap()
                .peer("box")
                .unwrap()
                .last_error_class,
            Some(FederationPollErrorClass::Transport)
        );

        peer.event_hub
            .push(peer_status("w1:p1", AgentStatus::Blocked));
        assert!(
            wait_until(Duration::from_secs(5), || hub_statuses(&relay.hub)
                == status_pairs(&[("box/w1:p1", AgentStatus::Blocked)])),
            "a transport miss must not stop the relay"
        );
        assert_eq!(peer.subscribes(), 1, "the stream was never dropped");

        relay.stop();
        peer.shutdown();
    }

    /// A status that arrives after a newer completed turn still reports the
    /// status change, but neither the published event nor the relay baseline
    /// goes back to its older turn.
    #[test]
    fn relay_never_publishes_or_records_a_rewound_turn() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let relay = start_relay(peer.addr);
        assert!(wait_until(Duration::from_secs(5), || peer.subscribes()
            == 1
            && relay_baseline_has(&relay.store, "box/w1:p1")));

        let pane: crate::api::schema::PaneInfo = serde_json::from_value(serde_json::json!({
            "pane_id": "w1:p1", "terminal_id": "t-w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
            "focused": false, "agent_status": "idle", "revision": 2,
        }))
        .unwrap();
        peer.event_hub.push(EventEnvelope {
            event: EventKind::PaneTurnCompleted,
            data: EventData::PaneTurnCompleted {
                pane,
                turn: 5,
                turn_epoch: 2,
                outcome: crate::terminal::TurnOutcome::Completed,
                message: None,
                message_truncated: false,
                agent_session_path: None,
                completed_unix_ms: 1,
            },
        });
        let mut late = peer_status("w1:p1", AgentStatus::Idle);
        if let EventData::PaneAgentStatusChanged {
            turn, turn_epoch, ..
        } = &mut late.data
        {
            (*turn, *turn_epoch) = (Some(4), Some(2));
        }
        peer.event_hub.push(late);
        assert!(wait_until(Duration::from_secs(5), || !hub_statuses(
            &relay.hub
        )
        .is_empty()));

        let published: Vec<_> = relay
            .hub
            .events_after(0)
            .into_iter()
            .filter_map(|(_, event)| match event.data {
                EventData::PaneAgentStatusChanged {
                    agent_status,
                    turn,
                    turn_epoch,
                    ..
                } => Some((agent_status, turn_epoch, turn)),
                _ => None,
            })
            .collect();
        assert_eq!(
            published,
            [(AgentStatus::Idle, Some(2), Some(5))],
            "the status change is published without rewinding the turn"
        );
        let baseline = relay.store.lock().unwrap().peer("box").unwrap().relayed["box/w1:p1"]
            .event
            .clone();
        assert_eq!((baseline.turn_epoch, baseline.turn), (Some(2), Some(5)));

        relay.stop();
        peer.shutdown();
    }

    /// A route generation change drops the stream and reconnects it, leaving
    /// exactly one live upstream stream; a peer restart with a new boot id is
    /// a full resync that publishes every pane even when its status is
    /// unchanged, since the old boot's pane ids mean nothing.
    #[test]
    fn relay_reconnects_on_stale_route_or_new_boot_with_one_stream() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            true,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let relay = start_relay(peer.addr);
        assert!(wait_until(Duration::from_secs(5), || peer.subscribes()
            == 1
            && relay_baseline_has(&relay.store, "box/w1:p1")));

        for round in 0..3 {
            let before = peer.subscribes();
            relay
                .route
                .force_remote_boot_for_test(&format!("boot-elsewhere-{round}"));
            assert!(
                wait_until(Duration::from_secs(5), || peer.subscribes() > before),
                "a stale route stamp must reconnect"
            );
        }
        std::thread::sleep(Duration::from_millis(2500));
        assert_eq!(
            peer.live_streams(Duration::from_millis(1500)).len(),
            1,
            "exactly one upstream stream survives the reconnects"
        );
        assert!(
            relay.hub.events_after(0).is_empty(),
            "reconnecting to the same boot publishes nothing new"
        );

        let addr = peer.shutdown();
        *state.boot_id.lock().unwrap() = "boot-2".into();
        let peer = StreamingPeer::spawn_on(addr, Arc::clone(&state));
        assert!(wait_until(Duration::from_secs(10), || !hub_statuses(
            &relay.hub
        )
        .is_empty()));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            hub_statuses(&relay.hub),
            status_pairs(&[("box/w1:p1", AgentStatus::Working)])
        );

        relay.stop();
        peer.shutdown();
    }

    /// A peer that does not advertise `events_v2` keeps the poll alone: its
    /// agents are cached but no event stream is ever opened.
    #[test]
    fn peer_without_events_v2_is_polled_but_never_streamed() {
        let state = streaming_state(
            vec![peer_agent("w1:p1", AgentStatus::Working)],
            "boot-1",
            false,
        );
        let peer = StreamingPeer::spawn(Arc::clone(&state));
        let token_path = unique_token_file(SEEDED_PEER_TOKEN);
        let cache = Arc::new(Mutex::new(FederationStore::default()));
        let hub = EventHub::default();
        let running = Arc::new(AtomicBool::new(true));
        let manager =
            FederationPeerManager::new(Arc::clone(&cache), hub.clone(), Arc::clone(&running));
        manager.reconcile(&[reachable_peer_on(peer.addr, "box", &token_path)]);
        assert!(wait_for_cached(&cache, "box", "box/w1:p1"));
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(peer.subscribes(), 0, "no stream without the capability");
        assert!(hub.events_after(0).is_empty());

        manager.join_all();
        peer.shutdown();
        let _ = std::fs::remove_file(&token_path);
    }
}
