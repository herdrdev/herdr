use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{ClientEndpointId, ClientEndpointStatus, EndpointNegotiation, NativeEndpointTransport};
use crate::protocol::{ClientSurfaceSize, RenderEncoding};
use interprocess::TryClone as _;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
pub(crate) struct EndpointConnectOptions {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) cell_width_px: u32,
    pub(crate) cell_height_px: u32,
    pub(crate) pixel_geometry_exact: bool,
    pub(crate) surface_size: ClientSurfaceSize,
    pub(crate) endpoint_keybindings: bool,
    pub(crate) mouse_capture: bool,
}

pub(crate) enum EndpointSupervisorEvent {
    Status {
        endpoint_id: ClientEndpointId,
        generation: u64,
        status: ClientEndpointStatus,
        message: String,
    },
    Connected {
        endpoint_id: ClientEndpointId,
        generation: u64,
        reader: crate::ipc::LocalStream,
        writer: NativeEndpointTransport,
        negotiation: EndpointNegotiation,
    },
}

#[derive(Clone)]
enum ConnectTarget {
    Local(PathBuf),
    Ssh(super::SavedSshEndpoint),
}

struct ReconnectState {
    target: ConnectTarget,
    attempts: u32,
    next_attempt: Option<Instant>,
    in_flight: bool,
    generation: Option<u64>,
}

impl ReconnectState {
    fn new(target: ConnectTarget, now: Instant) -> Self {
        Self {
            target,
            attempts: 0,
            next_attempt: Some(now),
            in_flight: false,
            generation: None,
        }
    }
}

pub(crate) struct EndpointSupervisors {
    endpoints: HashMap<ClientEndpointId, ReconnectState>,
    next_generation: u64,
    shutdown: Arc<AtomicBool>,
}

impl EndpointSupervisors {
    pub(crate) fn new(profiles: &[super::SavedSshEndpoint], now: Instant) -> Self {
        let endpoints = profiles
            .iter()
            .filter(|profile| profile.enabled)
            .map(|profile| {
                (
                    ClientEndpointId::Ssh(profile.id.clone()),
                    ReconnectState::new(ConnectTarget::Ssh(profile.clone()), now),
                )
            })
            .collect();
        Self {
            endpoints,
            next_generation: 2,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn add_local(&mut self, path: PathBuf, generation: Option<u64>, now: Instant) {
        let mut state = ReconnectState::new(ConnectTarget::Local(path), now);
        state.generation = generation;
        if generation.is_some() {
            state.next_attempt = None;
        }
        self.endpoints.insert(ClientEndpointId::Local, state);
    }

    pub(crate) fn reconcile_profiles(
        &mut self,
        profiles: &[super::SavedSshEndpoint],
        now: Instant,
    ) -> Vec<ClientEndpointId> {
        let mut retired = Vec::new();
        self.endpoints.retain(|endpoint_id, state| {
            let ConnectTarget::Ssh(previous) = &state.target else {
                return true;
            };
            let keep = profiles.iter().any(|profile| {
                profile.id == previous.id
                    && profile.enabled
                    && profile.target == previous.target
                    && profile.session == previous.session
                    && profile.windows_desktop == previous.windows_desktop
            });
            if !keep {
                retired.push(endpoint_id.clone());
            }
            keep
        });
        for profile in profiles.iter().filter(|profile| profile.enabled) {
            let state = self
                .endpoints
                .entry(ClientEndpointId::Ssh(profile.id.clone()))
                .or_insert_with(|| ReconnectState::new(ConnectTarget::Ssh(profile.clone()), now));
            state.target = ConnectTarget::Ssh(profile.clone());
        }
        retired
    }

    pub(crate) fn spawn_due(
        &mut self,
        now: Instant,
        options: EndpointConnectOptions,
        event_tx: &tokio::sync::mpsc::Sender<EndpointSupervisorEvent>,
    ) {
        for (endpoint_id, state) in &mut self.endpoints {
            if state.in_flight || state.next_attempt.is_none_or(|deadline| deadline > now) {
                continue;
            }
            state.in_flight = true;
            state.next_attempt = None;
            let generation = self.next_generation;
            state.generation = Some(generation);
            self.next_generation = self.next_generation.saturating_add(1);
            let endpoint_id = endpoint_id.clone();
            let target = state.target.clone();
            let event_tx = event_tx.clone();
            let shutdown = self.shutdown.clone();
            tokio::spawn(async move {
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                let task_endpoint_id = endpoint_id.clone();
                let result = tokio::task::spawn_blocking(move || {
                    connect_once(&target, options, endpoint_id, generation)
                })
                .await;
                let event = match result {
                    Ok(Ok(event)) => event,
                    Ok(Err(error)) => EndpointSupervisorEvent::Status {
                        endpoint_id: task_endpoint_id,
                        generation,
                        status: if failure_needs_attention(&error) {
                            ClientEndpointStatus::Attention
                        } else {
                            ClientEndpointStatus::Reconnecting
                        },
                        message: error.to_string(),
                    },
                    Err(error) => EndpointSupervisorEvent::Status {
                        endpoint_id: task_endpoint_id,
                        generation,
                        status: ClientEndpointStatus::Reconnecting,
                        message: format!("endpoint connection task stopped unexpectedly: {error}"),
                    },
                };
                if !shutdown.load(Ordering::Acquire) {
                    let _ = event_tx.send(event).await;
                }
            });
        }
    }

    pub(crate) fn record_status(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        status: ClientEndpointStatus,
        now: Instant,
    ) -> bool {
        let Some(state) = self.endpoints.get_mut(endpoint_id) else {
            return false;
        };
        if state.generation != Some(generation) {
            return false;
        }
        state.in_flight = false;
        match status {
            ClientEndpointStatus::Online => {
                state.attempts = 0;
                state.next_attempt = None;
            }
            ClientEndpointStatus::Attention | ClientEndpointStatus::Disabled => {
                state.next_attempt = None
            }
            ClientEndpointStatus::Connecting | ClientEndpointStatus::Reconnecting => {
                state.attempts = state.attempts.saturating_add(1);
                state.next_attempt = Some(now + retry_delay(state.attempts));
            }
        }
        true
    }

    pub(crate) fn disconnected(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        now: Instant,
    ) -> bool {
        self.record_status(
            endpoint_id,
            generation,
            ClientEndpointStatus::Reconnecting,
            now,
        )
    }
}

impl Drop for EndpointSupervisors {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

fn connect_once(
    target: &ConnectTarget,
    options: EndpointConnectOptions,
    endpoint_id: ClientEndpointId,
    generation: u64,
) -> Result<EndpointSupervisorEvent, std::io::Error> {
    let (mut stream, lifetime): (_, Box<dyn Send>) = match target {
        ConnectTarget::Local(path) => {
            let stream = crate::ipc::connect_local_stream(path).map_err(|error| {
                // An absent Local socket is transient, unlike a missing SSH install.
                if error.kind() == std::io::ErrorKind::NotFound {
                    std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "Local is unavailable; start its server to reconnect",
                    )
                } else {
                    error
                }
            })?;
            (stream, Box::new(()))
        }
        ConnectTarget::Ssh(profile) => {
            let connected = crate::remote::connect_saved_ssh(profile.id.as_str(), &profile.target, &profile.session, profile.windows_desktop).map_err(|error| {
                if failure_needs_attention(&error) {
                    std::io::Error::new(error.kind(), format!("{error}. Run `{}` interactively to approve setup, then restart this client", crate::remote::saved_ssh_bootstrap_command(&profile.target, &profile.session, profile.windows_desktop)))
                } else { error }
            })?;
            (connected.stream, Box::new(connected.bridge))
        }
    };
    let handshake = super::super::do_handshake(
        &mut stream,
        options.cols,
        options.rows,
        options.cell_width_px,
        options.cell_height_px,
        options.pixel_geometry_exact,
        Some(options.surface_size),
        options.endpoint_keybindings,
        options.mouse_capture,
        false,
    )
    .map_err(handshake_error)?;
    if handshake.encoding != RenderEncoding::SemanticFrame {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "endpoint did not negotiate the semantic client shell",
        ));
    }
    let negotiation = EndpointNegotiation::new(
        handshake.endpoint_methods.unwrap_or_default(),
        handshake.endpoint_capabilities.unwrap_or_default(),
    );
    if !negotiation.supports_surface_interest()
        || (!endpoint_id.is_local() && !negotiation.supports_health_check())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this machine needs a server update before it can participate in multi-machine viewing",
        ));
    }
    let reader = stream.try_clone()?;
    let writer = NativeEndpointTransport::with_lifetime(stream, lifetime)?;
    Ok(EndpointSupervisorEvent::Connected {
        endpoint_id,
        generation,
        reader,
        writer,
        negotiation,
    })
}

fn failure_needs_attention(error: &std::io::Error) -> bool {
    crate::remote::saved_ssh_failure_needs_attention(error)
}

fn handshake_error(error: crate::client::ClientError) -> std::io::Error {
    use crate::client::ClientError;
    use crate::protocol::FramingError;
    match error {
        ClientError::ConnectionFailed(error) | ClientError::ConnectionLost(error) => error,
        ClientError::HandshakeRejected { error, .. } => {
            std::io::Error::new(std::io::ErrorKind::Unsupported, error)
        }
        ClientError::Protocol(FramingError::Io(error)) => error,
        ClientError::Protocol(error) => {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        }
        ClientError::ServerShutdown { reason } => std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            reason.unwrap_or_else(|| "server shut down during handshake".into()),
        ),
    }
}

fn retry_delay(attempt: u32) -> Duration {
    INITIAL_RETRY_DELAY
        .saturating_mul(
            1_u32
                .checked_shl(attempt.saturating_sub(1).min(6))
                .unwrap_or(u32::MAX),
        )
        .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::endpoint::ProfileId;

    fn profile() -> super::super::SavedSshEndpoint {
        super::super::SavedSshEndpoint {
            id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            label: "Build".into(),
            target: "build".into(),
            session: "agents".into(),
            enabled: true,
            windows_desktop: false,
        }
    }

    #[test]
    fn live_catalog_preserves_renamed_connections_and_local_recovery() {
        let now = Instant::now();
        let mut profile = profile();
        let id = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[profile.clone()], now);
        supervisors.add_local(PathBuf::from("local"), Some(1), now);
        let state = supervisors.endpoints.get_mut(&id).unwrap();
        state.generation = Some(7);
        state.attempts = 3;
        state.next_attempt = Some(now + Duration::from_secs(4));
        profile.label = "Renamed".into();
        assert!(supervisors.reconcile_profiles(&[profile], now).is_empty());
        let state = &supervisors.endpoints[&id];
        assert_eq!(state.generation, Some(7));
        assert_eq!(state.attempts, 3);
        assert_eq!(state.next_attempt, Some(now + Duration::from_secs(4)));
        assert_eq!(
            supervisors.endpoints[&ClientEndpointId::Local].generation,
            Some(1)
        );
    }

    #[test]
    fn live_catalog_add_disable_enable_and_remove_fence_late_connections() {
        let now = Instant::now();
        let mut profile = profile();
        let id = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[], now);
        assert!(supervisors
            .reconcile_profiles(&[profile.clone()], now)
            .is_empty());
        assert_eq!(supervisors.endpoints[&id].next_attempt, Some(now));
        supervisors.endpoints.get_mut(&id).unwrap().generation = Some(7);
        supervisors.next_generation = 8;
        profile.enabled = false;
        assert_eq!(
            supervisors.reconcile_profiles(&[profile.clone()], now),
            vec![id.clone()]
        );
        assert!(!supervisors.record_status(&id, 7, ClientEndpointStatus::Online, now));
        profile.enabled = true;
        assert!(supervisors
            .reconcile_profiles(&[profile.clone()], now)
            .is_empty());
        assert!(!supervisors.record_status(&id, 7, ClientEndpointStatus::Online, now));
        assert_eq!(supervisors.next_generation, 8);
        assert_eq!(supervisors.reconcile_profiles(&[], now), vec![id.clone()]);
        assert!(!supervisors.record_status(&id, 7, ClientEndpointStatus::Online, now));
    }

    #[test]
    fn live_catalog_destination_change_retires_only_that_machine() {
        let now = Instant::now();
        let mut changed = profile();
        let other = super::super::SavedSshEndpoint::new("Other", "other", "main").unwrap();
        let id = ClientEndpointId::Ssh(changed.id.clone());
        let other_id = ClientEndpointId::Ssh(other.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[changed.clone(), other.clone()], now);
        supervisors.endpoints.get_mut(&id).unwrap().generation = Some(2);
        supervisors.endpoints.get_mut(&other_id).unwrap().generation = Some(3);
        changed.session = "another-session".into();
        assert_eq!(
            supervisors.reconcile_profiles(&[changed, other], now),
            vec![id.clone()]
        );
        assert_eq!(supervisors.endpoints[&id].generation, None);
        assert_eq!(supervisors.endpoints[&other_id].generation, Some(3));
    }

    #[test]
    fn retry_backoff_is_bounded() {
        assert_eq!(retry_delay(1), INITIAL_RETRY_DELAY);
        assert_eq!(retry_delay(100), MAX_RETRY_DELAY);
    }

    #[test]
    fn handshake_network_failures_retry_but_incompatibility_needs_attention() {
        let timeout = handshake_error(crate::client::ClientError::ConnectionLost(
            std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"),
        ));
        assert!(!failure_needs_attention(&timeout));
        let rejected = handshake_error(crate::client::ClientError::HandshakeRejected {
            version: 1,
            error: "surface capability missing".into(),
        });
        assert_eq!(rejected.kind(), std::io::ErrorKind::Unsupported);
        assert!(failure_needs_attention(&rejected));
    }

    #[test]
    fn healthy_local_only_retries_after_its_connection_fails() {
        let now = Instant::now();
        let mut supervisors = EndpointSupervisors::new(&[profile()], now);
        supervisors.add_local(PathBuf::from("local.sock"), Some(1), now);
        assert!(supervisors.endpoints[&ClientEndpointId::Local]
            .next_attempt
            .is_none());
        assert!(!supervisors.disconnected(&ClientEndpointId::Local, 0, now));
        assert!(supervisors.endpoints[&ClientEndpointId::Local]
            .next_attempt
            .is_none());
        assert!(supervisors.disconnected(&ClientEndpointId::Local, 1, now));
        assert_eq!(
            supervisors.endpoints[&ClientEndpointId::Local].next_attempt,
            Some(now + INITIAL_RETRY_DELAY)
        );
        assert_eq!(
            supervisors.endpoints[&ClientEndpointId::Ssh(profile().id)].next_attempt,
            Some(now)
        );
    }

    #[test]
    fn ssh_recovery_rejects_stale_generations_and_stops_retries_for_attention() {
        let now = Instant::now();
        let mut supervisors = EndpointSupervisors::new(&[profile()], now);
        let endpoint_id = ClientEndpointId::Ssh(profile().id);
        supervisors
            .endpoints
            .get_mut(&endpoint_id)
            .unwrap()
            .generation = Some(4);
        assert!(supervisors.record_status(&endpoint_id, 4, ClientEndpointStatus::Online, now));
        assert!(!supervisors.disconnected(&endpoint_id, 3, now));
        assert!(supervisors.endpoints[&endpoint_id].next_attempt.is_none());
        assert!(supervisors.disconnected(&endpoint_id, 4, now));
        assert_eq!(
            supervisors.endpoints[&endpoint_id].next_attempt,
            Some(now + INITIAL_RETRY_DELAY)
        );
        assert!(supervisors.record_status(&endpoint_id, 4, ClientEndpointStatus::Attention, now));
        assert!(supervisors.endpoints[&endpoint_id].next_attempt.is_none());
    }
}
