use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{ClientEndpointId, ClientEndpointStatus, EndpointNegotiation, NativeEndpointTransport};
use crate::protocol::{ClientSurfaceSize, RenderEncoding};
use interprocess::TryClone as _;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(120);
const MAX_LOCAL_RETRY_DELAY: Duration = Duration::from_secs(30);
const STABLE_CONNECTION_PERIOD: Duration = Duration::from_secs(60);
// Allow Wi-Fi to return without restarting the historical failure ladder.
const WAKE_RECOVERY_PERIOD: Duration = Duration::from_secs(30);
const WAKE_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
pub(crate) struct EndpointConnectOptions {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) cell_width_px: u32,
    pub(crate) cell_height_px: u32,
    pub(crate) pixel_geometry_exact: bool,
    pub(crate) pixel_mouse: bool,
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
    cancelled: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    generation: Option<u64>,
    ready_since: Option<Instant>,
    retry_after_resume: bool,
}

impl ReconnectState {
    fn new(target: ConnectTarget, now: Instant) -> Self {
        Self {
            target,
            attempts: 0,
            next_attempt: Some(now),
            in_flight: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(AtomicBool::new(true)),
            generation: None,
            ready_since: None,
            retry_after_resume: false,
        }
    }
}

impl Drop for ReconnectState {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct AttemptCompletion(Arc<AtomicBool>);

impl Drop for AttemptCompletion {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) struct EndpointSupervisors {
    endpoints: HashMap<ClientEndpointId, ReconnectState>,
    retired: HashMap<ClientEndpointId, Arc<AtomicBool>>,
    next_generation: u64,
    shutdown: Arc<AtomicBool>,
    power_state: Option<crate::platform::HostPowerEvent>,
    wake_recovery_until: Option<Instant>,
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
            retired: HashMap::new(),
            next_generation: 2,
            shutdown: Arc::new(AtomicBool::new(false)),
            power_state: None,
            wake_recovery_until: None,
        }
    }

    pub(crate) fn add_local(&mut self, path: PathBuf, generation: Option<u64>, now: Instant) {
        let mut state = ReconnectState::new(ConnectTarget::Local(path), now);
        state.generation = generation;
        if generation.is_some() {
            state.next_attempt = None;
        }
        if let Some(previous) = self.endpoints.insert(ClientEndpointId::Local, state) {
            if !previous.completed.load(Ordering::Acquire) {
                self.retired
                    .entry(ClientEndpointId::Local)
                    .or_insert_with(|| Arc::clone(&previous.completed));
            }
        }
    }

    pub(crate) fn reconcile_profiles(
        &mut self,
        profiles: &[super::SavedSshEndpoint],
        now: Instant,
    ) -> Vec<ClientEndpointId> {
        let mut retired = Vec::new();
        let retiring = &mut self.retired;
        self.endpoints.retain(|endpoint_id, state| {
            let ConnectTarget::Ssh(previous) = &state.target else {
                return true;
            };
            let keep = profiles.iter().any(|profile| {
                profile.id == previous.id
                    && profile.enabled
                    && profile.target == previous.target
                    && profile.session == previous.session
            });
            if !keep {
                retired.push(endpoint_id.clone());
                // A stale Connected event still owns its bridge until the client rejects it.
                if !state.completed.load(Ordering::Acquire) {
                    retiring
                        .entry(endpoint_id.clone())
                        .or_insert_with(|| Arc::clone(&state.completed));
                }
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
        if self.power_state == Some(crate::platform::HostPowerEvent::Suspend) {
            return;
        }
        self.retired
            .retain(|_, completed| !completed.load(Ordering::Acquire));
        for (endpoint_id, state) in &mut self.endpoints {
            if state.cancelled.load(Ordering::Acquire) && state.completed.load(Ordering::Acquire) {
                state.in_flight = false;
            }
            if state.in_flight
                || !state.completed.load(Ordering::Acquire)
                || self.retired.contains_key(endpoint_id)
                || state.next_attempt.is_none_or(|deadline| deadline > now)
            {
                continue;
            }
            state.in_flight = true;
            state.cancelled = Arc::new(AtomicBool::new(false));
            let cancelled = Arc::clone(&state.cancelled);
            state.completed = Arc::new(AtomicBool::new(false));
            let completed = Arc::clone(&state.completed);
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
                    completed.store(true, Ordering::Release);
                    return;
                }
                let task_endpoint_id = endpoint_id.clone();
                let result = tokio::task::spawn_blocking(move || {
                    connect_once(
                        &target,
                        options,
                        endpoint_id,
                        generation,
                        cancelled,
                        completed,
                    )
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
        let retry_after_resume = std::mem::take(&mut state.retry_after_resume);
        match status {
            ClientEndpointStatus::Online => {
                if endpoint_id.is_local() {
                    state.attempts = 0;
                }
                state.next_attempt = None;
            }
            ClientEndpointStatus::Attention => {
                state.ready_since = None;
                // Authentication or configuration may be repaired outside this client.
                state.next_attempt =
                    (!endpoint_id.is_local()).then_some(now + Duration::from_secs(30));
            }
            ClientEndpointStatus::Disabled => {
                state.ready_since = None;
                state.next_attempt = None;
            }
            ClientEndpointStatus::Connecting | ClientEndpointStatus::Reconnecting => {
                // A brief maintenance wake can complete a handshake without restoring the link.
                if state.ready_since.take().is_some_and(|ready| {
                    now.saturating_duration_since(ready) >= STABLE_CONNECTION_PERIOD
                }) {
                    state.attempts = 0;
                }
                state.attempts = state.attempts.saturating_add(1);
                let delay = retry_delay(state.attempts);
                let delay = if retry_after_resume {
                    Duration::ZERO
                } else if self.wake_recovery_until.is_some_and(|until| now < until) {
                    delay.min(WAKE_RETRY_DELAY)
                } else {
                    delay
                };
                state.next_attempt = Some(
                    now + if endpoint_id.is_local() {
                        delay.min(MAX_LOCAL_RETRY_DELAY)
                    } else {
                        delay
                    },
                );
            }
        }
        true
    }

    /// Returns whether live connections need a fresh probe, without replacing them.
    pub(crate) fn host_power_event(
        &mut self,
        event: crate::platform::HostPowerEvent,
        now: Instant,
    ) -> bool {
        use crate::platform::HostPowerEvent::*;
        if event == Suspend {
            if self.power_state == Some(Suspend) {
                return false;
            }
            self.power_state = Some(Suspend);
            self.wake_recovery_until = None;
            self.cancel_connecting(now);
            return false;
        }
        if (event == InteractiveResume && self.wake_recovery_until.is_some())
            || (event != InteractiveResume
                && self.power_state.is_some_and(|state| state != Suspend))
        {
            return false;
        }
        if self.power_state.is_none() {
            // Registration may have missed the preceding suspend notification.
            self.cancel_connecting(now);
        }
        self.power_state = Some(event);
        if event == InteractiveResume {
            self.wake_recovery_until = Some(now + WAKE_RECOVERY_PERIOD);
        }
        if matches!(event, InteractiveResume | UnclassifiedResume) {
            for state in self.endpoints.values_mut() {
                // A live transport may discover its wake failure only after the fresh probe.
                state.retry_after_resume = event == UnclassifiedResume
                    && state.next_attempt.is_none()
                    && state.generation.is_some();
                if state.next_attempt.is_some() {
                    state.next_attempt = Some(now);
                }
            }
        }
        true
    }

    fn cancel_connecting(&mut self, now: Instant) {
        for state in self.endpoints.values_mut() {
            state.ready_since = None;
            state.retry_after_resume = false;
            if state.in_flight {
                state.cancelled.store(true, Ordering::Release);
                state.generation = None;
                state.next_attempt = Some(now);
            }
        }
    }

    #[cfg(any(unix, test))]
    pub(crate) fn user_activity(&mut self, now: Instant) {
        if self.power_state == Some(crate::platform::HostPowerEvent::UnclassifiedResume) {
            self.host_power_event(crate::platform::HostPowerEvent::InteractiveResume, now);
        }
    }

    pub(crate) fn received(&mut self, endpoint_id: &ClientEndpointId, generation: u64) {
        if let Some(state) = self
            .endpoints
            .get_mut(endpoint_id)
            .filter(|state| state.generation == Some(generation))
        {
            state.retry_after_resume = false;
        }
    }

    pub(crate) fn mark_ready(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        now: Instant,
    ) {
        if let Some(state) = self
            .endpoints
            .get_mut(endpoint_id)
            .filter(|state| state.generation == Some(generation))
        {
            state.ready_since.get_or_insert(now);
        }
    }

    /// A user asked for this machine now, so skip its pending backoff and restart the ladder.
    /// Returns false when no attempt is waiting: it is online, in flight, or not retried at all.
    pub(crate) fn retry_now(&mut self, endpoint_id: &ClientEndpointId, now: Instant) -> bool {
        let Some(state) = self.endpoints.get_mut(endpoint_id) else {
            return false;
        };
        if state.in_flight || state.next_attempt.is_none() {
            return false;
        }
        state.attempts = 0;
        state.next_attempt = Some(now);
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
    cancelled: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
) -> Result<EndpointSupervisorEvent, std::io::Error> {
    let completion = AttemptCompletion(completed);
    if cancelled.load(Ordering::Acquire) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "connection attempt cancelled",
        ));
    }
    let (mut stream, lifetime): (_, Box<dyn Send>) = match target {
        ConnectTarget::Local(path) => {
            let stream = crate::ipc::connect_local_stream_cancellable(
                path,
                &cancelled,
                Instant::now() + super::super::handshake::LOCAL_HANDSHAKE_READ_TIMEOUT,
            )
            .map_err(|error| {
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
            let connected = crate::remote::connect_saved_ssh(
                profile.id.as_str(),
                &profile.target,
                &profile.session,
                Arc::clone(&cancelled),
            )?;
            (connected.stream, Box::new(connected.bridge))
        }
    };
    let handshake = super::super::do_handshake(
        &mut stream,
        options.cols,
        options.rows,
        options.cell_width_px,
        options.cell_height_px,
        super::super::HandshakePixelCapabilities {
            exact_geometry: options.pixel_geometry_exact,
            sgr_mouse: options.pixel_mouse,
        },
        Some(options.surface_size),
        options.endpoint_keybindings,
        options.mouse_capture,
        false,
        matches!(target, ConnectTarget::Local(_)),
        &cancelled,
    )
    .map_err(handshake_error)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "connection attempt cancelled",
        ));
    }
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
    // The completion barrier follows the bridge through queued and active transports.
    let writer = NativeEndpointTransport::with_lifetime(stream, (lifetime, completion))?;
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
                .checked_shl(attempt.saturating_sub(1).min(8))
                .unwrap_or(u32::MAX),
        )
        .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect_options() -> EndpointConnectOptions {
        EndpointConnectOptions {
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_geometry_exact: false,
            pixel_mouse: false,
            surface_size: ClientSurfaceSize { cols: 80, rows: 24 },
            endpoint_keybindings: false,
            mouse_capture: false,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replacement_waits_for_queued_connection_teardown() {
        use crate::protocol::endpoint::{EndpointServerWelcome, ENDPOINT_WELCOME_KIND};
        use crate::protocol::{self, ClientMessage, ServerMessage};
        #[cfg(unix)]
        use interprocess::local_socket::traits::Listener as _;
        use interprocess::local_socket::traits::Stream as _;
        let path = std::env::temp_dir().join(format!("hretired-{}.sock", std::process::id()));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let now = Instant::now();
        let mut supervisors = EndpointSupervisors::new(&[], now);
        supervisors.add_local(path.clone(), None, now);
        let (events, mut received) = tokio::sync::mpsc::channel(4);
        supervisors.spawn_due(now, connect_options(), &events);
        let mut peer = listener.accept().unwrap();
        let _: ClientMessage = protocol::read_message(&mut peer, protocol::MAX_FRAME_SIZE).unwrap();
        protocol::write_message(
            &mut peer,
            &ServerMessage::EndpointControl {
                kind: ENDPOINT_WELCOME_KIND.into(),
                data: serde_json::to_string(&EndpointServerWelcome::compatible(vec![
                    "client_shell.surface.set".into(),
                ]))
                .unwrap(),
            },
        )
        .unwrap();
        let queued = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        let EndpointSupervisorEvent::Connected { generation, .. } = &queued else {
            panic!("handshake failed");
        };
        let old_generation = *generation;
        // Repeated catalog replacements must keep the original unfinished barrier.
        for _ in 0..3 {
            supervisors.add_local(path.clone(), None, now);
            supervisors.spawn_due(now, connect_options(), &events);
            assert!(supervisors.endpoints[&ClientEndpointId::Local]
                .generation
                .is_none());
            assert_eq!(supervisors.retired.len(), 1);
        }
        assert!(!supervisors.record_status(
            &ClientEndpointId::Local,
            old_generation,
            ClientEndpointStatus::Online,
            now
        ));
        peer.set_nonblocking(true).unwrap();
        assert!(matches!(
            crate::ipc::poll_local_stream_read_count(&mut peer, &mut [0; 1]).unwrap(),
            crate::ipc::LocalStreamReadCount::Pending
        ));
        drop(queued);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            supervisors.spawn_due(now, connect_options(), &events);
            if supervisors.endpoints[&ClientEndpointId::Local]
                .generation
                .is_some()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "retired transport did not release its attempt"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(supervisors.retired.is_empty());
        assert!(matches!(
            crate::ipc::poll_local_stream_read_count(&mut peer, &mut [0; 1]).unwrap(),
            crate::ipc::LocalStreamReadCount::Closed
        ));
        let _replacement = listener.accept().unwrap();
        drop(supervisors);
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wake_replacement_waits_for_the_obsolete_attempt_to_close() {
        use crate::platform::HostPowerEvent::*;
        use crate::protocol::{self, ClientMessage};
        #[cfg(unix)]
        use interprocess::local_socket::traits::Listener as _;
        use interprocess::local_socket::traits::Stream as _;
        for observed_suspend in [true, false] {
            let path = std::env::temp_dir().join(format!(
                "hwake-{}-{observed_suspend}.sock",
                std::process::id()
            ));
            let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
            let now = Instant::now();
            let mut supervisors = EndpointSupervisors::new(&[], now);
            supervisors.add_local(path.clone(), None, now);
            let (events, mut received) = tokio::sync::mpsc::channel(4);
            supervisors.spawn_due(now, connect_options(), &events);
            let mut peer = listener.accept().unwrap();
            let _: ClientMessage =
                protocol::read_message(&mut peer, protocol::MAX_FRAME_SIZE).unwrap();
            let completed = supervisors.endpoints[&ClientEndpointId::Local]
                .completed
                .clone();
            supervisors.host_power_event(
                if observed_suspend {
                    Suspend
                } else {
                    InteractiveResume
                },
                now,
            );
            assert!(supervisors.endpoints[&ClientEndpointId::Local].in_flight);
            if observed_suspend {
                supervisors.spawn_due(now, connect_options(), &events);
            }
            assert!(supervisors.endpoints[&ClientEndpointId::Local]
                .generation
                .is_none());
            let status = tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap()
                .unwrap();
            let EndpointSupervisorEvent::Status {
                endpoint_id,
                generation,
                status,
                ..
            } = status
            else {
                panic!("cancelled attempt connected");
            };
            assert!(completed.load(Ordering::Acquire));
            assert!(!supervisors.record_status(&endpoint_id, generation, status, now));
            peer.set_nonblocking(true).unwrap();
            // Windows interprocess drops flush through its background linger pool.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if matches!(
                    crate::ipc::poll_local_stream_read_count(&mut peer, &mut [0; 1]).unwrap(),
                    crate::ipc::LocalStreamReadCount::Closed
                ) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "cancelled attempt retained its stream"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            supervisors.host_power_event(InteractiveResume, now);
            supervisors.spawn_due(now, connect_options(), &events);
            let replacement = supervisors.endpoints[&ClientEndpointId::Local].generation;
            assert_ne!(replacement, Some(generation));
            assert!(replacement.is_some());
            for _ in 0..3 {
                supervisors.host_power_event(InteractiveResume, now);
                supervisors.spawn_due(now, connect_options(), &events);
                assert_eq!(
                    supervisors.endpoints[&ClientEndpointId::Local].generation,
                    replacement
                );
            }
            let _replacement = listener.accept().unwrap();
            drop(supervisors);
            drop(listener);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retiring_or_shutting_down_a_silent_attempt_closes_its_connection() {
        use crate::protocol::{self, ClientMessage};
        #[cfg(unix)]
        use interprocess::local_socket::traits::Listener as _;
        use interprocess::local_socket::traits::Stream as _;
        for shutdown in [false, true] {
            let path = std::env::temp_dir()
                .join(format!("hcancel-{}-{shutdown}.sock", std::process::id()));
            let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
            let now = Instant::now();
            let mut supervisors = EndpointSupervisors::new(&[], now);
            supervisors.add_local(path.clone(), None, now);
            let (events, mut received) = tokio::sync::mpsc::channel(4);
            supervisors.spawn_due(now, connect_options(), &events);
            let mut peer = listener.accept().unwrap();
            let _: ClientMessage =
                protocol::read_message(&mut peer, protocol::MAX_FRAME_SIZE).unwrap();
            drop(listener);
            std::fs::remove_file(path).unwrap();
            let started = Instant::now();
            if shutdown {
                drop(supervisors);
            } else {
                supervisors.add_local(PathBuf::from("replacement"), None, now);
                let event = tokio::time::timeout(Duration::from_secs(2), received.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let EndpointSupervisorEvent::Status {
                    endpoint_id,
                    generation,
                    status,
                    ..
                } = event
                else {
                    panic!("cancelled attempt connected");
                };
                assert!(!supervisors.record_status(&endpoint_id, generation, status, now));
            }
            peer.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if matches!(
                    crate::ipc::poll_local_stream_read_count(&mut peer, &mut [0; 1]).unwrap(),
                    crate::ipc::LocalStreamReadCount::Closed
                ) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "cancelled attempt retained its stream"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
    use crate::client::endpoint::ProfileId;

    fn profile() -> super::super::SavedSshEndpoint {
        super::super::SavedSshEndpoint {
            id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            label: "Build".into(),
            target: "build".into(),
            session: "agents".into(),
            enabled: true,
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
    fn brief_ssh_reconnections_do_not_reset_backoff() {
        let now = Instant::now();
        let profile = profile();
        let id = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[profile], now);
        supervisors.endpoints.get_mut(&id).unwrap().generation = Some(2);
        for attempt in 1..=5 {
            let connected = now + Duration::from_secs(attempt * 20);
            assert!(supervisors.record_status(&id, 2, ClientEndpointStatus::Online, connected));
            supervisors.mark_ready(&id, 2, connected);
            let failed = connected + Duration::from_secs(15);
            assert!(supervisors.disconnected(&id, 2, failed));
            assert_eq!(
                supervisors.endpoints[&id].next_attempt,
                Some(failed + INITIAL_RETRY_DELAY * (1 << (attempt - 1)))
            );
        }
        let connected = now + Duration::from_secs(200);
        assert!(supervisors.record_status(&id, 2, ClientEndpointStatus::Online, connected));
        supervisors.mark_ready(&id, 1, connected);
        let ready = connected + Duration::from_secs(45);
        supervisors.mark_ready(&id, 2, ready);
        let failed = connected + Duration::from_secs(60);
        assert!(supervisors.disconnected(&id, 2, failed));
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(failed + Duration::from_secs(16))
        );

        let connected = now + Duration::from_secs(300);
        assert!(supervisors.record_status(&id, 2, ClientEndpointStatus::Online, connected));
        let ready = connected + Duration::from_secs(45);
        supervisors.mark_ready(&id, 2, ready);
        supervisors.mark_ready(&id, 2, ready + Duration::from_secs(30));
        let failed = ready + Duration::from_secs(60);
        assert!(supervisors.disconnected(&id, 2, failed));
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(failed + INITIAL_RETRY_DELAY)
        );
    }

    #[test]
    fn a_handshake_without_readiness_does_not_reset_backoff() {
        let now = Instant::now();
        let profile = profile();
        let id = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[profile], now);
        let state = supervisors.endpoints.get_mut(&id).unwrap();
        state.generation = Some(2);
        state.attempts = 9;
        assert!(supervisors.record_status(&id, 2, ClientEndpointStatus::Online, now));
        let failed = now + Duration::from_secs(75);
        assert!(supervisors.disconnected(&id, 2, failed));
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(failed + MAX_RETRY_DELAY)
        );
    }

    #[test]
    fn requested_retry_skips_backoff_without_duplicating_attempts() {
        let now = Instant::now();
        let profile = profile();
        let id = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[profile], now);
        let state = supervisors.endpoints.get_mut(&id).unwrap();
        state.generation = Some(2);
        state.attempts = 9;
        assert!(supervisors.disconnected(&id, 2, now));
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(now + MAX_RETRY_DELAY)
        );

        let clicked = now + Duration::from_secs(5);
        assert!(supervisors.retry_now(&id, clicked));
        assert_eq!(supervisors.endpoints[&id].next_attempt, Some(clicked));
        // A failed requested attempt resumes from the shortest delay.
        assert!(supervisors.disconnected(&id, 2, clicked));
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(clicked + INITIAL_RETRY_DELAY)
        );

        supervisors.endpoints.get_mut(&id).unwrap().in_flight = true;
        assert!(!supervisors.retry_now(&id, clicked));
        assert!(supervisors.record_status(&id, 2, ClientEndpointStatus::Online, clicked));
        assert!(!supervisors.retry_now(&id, clicked));
        assert!(supervisors.endpoints[&id].next_attempt.is_none());
    }

    #[test]
    fn interactive_wake_retries_slow_wifi_without_renewing_the_window_or_failure_history() {
        use crate::platform::HostPowerEvent::*;
        let now = Instant::now();
        let profile = profile();
        let id = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[profile], now);
        let state = supervisors.endpoints.get_mut(&id).unwrap();
        state.generation = Some(2);
        state.attempts = 9;
        state.next_attempt = Some(now + Duration::from_secs(120));
        supervisors.host_power_event(Suspend, now);
        assert!(supervisors.host_power_event(AutomaticResume, now));
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(now + Duration::from_secs(120))
        );
        assert!(supervisors.host_power_event(InteractiveResume, now));
        assert_eq!(supervisors.endpoints[&id].next_attempt, Some(now));
        assert_eq!(supervisors.endpoints[&id].attempts, 9);
        // Wi-Fi remains unavailable for twenty seconds after the interactive wake.
        for seconds in (0..=20).step_by(2) {
            let failed = now + Duration::from_secs(seconds);
            assert!(supervisors.disconnected(&id, 2, failed));
            assert_eq!(
                supervisors.endpoints[&id].next_attempt,
                Some(failed + Duration::from_secs(2))
            );
        }
        let failures = supervisors.endpoints[&id].attempts;
        let late = now + Duration::from_secs(31);
        for event in [
            AutomaticResume,
            InteractiveResume,
            UnclassifiedResume,
            InteractiveResume,
        ] {
            assert!(!supervisors.host_power_event(event, late));
        }
        supervisors.disconnected(&id, 2, late);
        assert_eq!(supervisors.endpoints[&id].attempts, failures + 1);
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(late + Duration::from_secs(120))
        );
        supervisors.host_power_event(Suspend, late);
        assert!(supervisors.host_power_event(InteractiveResume, late));
        assert_eq!(supervisors.endpoints[&id].next_attempt, Some(late));
    }

    #[test]
    fn unclassified_resume_expedites_once_until_user_activity() {
        use crate::platform::HostPowerEvent::*;
        let now = Instant::now();
        let profile = profile();
        let id = ClientEndpointId::Ssh(profile.id.clone());
        let mut supervisors = EndpointSupervisors::new(&[profile], now);
        let state = supervisors.endpoints.get_mut(&id).unwrap();
        state.generation = Some(2);
        state.attempts = 9;
        state.next_attempt = Some(now + Duration::from_secs(120));
        supervisors.host_power_event(Suspend, now);
        supervisors.host_power_event(UnclassifiedResume, now);
        assert_eq!(supervisors.endpoints[&id].next_attempt, Some(now));
        supervisors.disconnected(&id, 2, now);
        assert!(!supervisors.host_power_event(UnclassifiedResume, now));
        assert_eq!(
            supervisors.endpoints[&id].next_attempt,
            Some(now + Duration::from_secs(120))
        );
        supervisors.user_activity(now);
        assert_eq!(supervisors.endpoints[&id].next_attempt, Some(now));
        let deadline = supervisors.wake_recovery_until;
        supervisors.user_activity(now + Duration::from_secs(20));
        supervisors.host_power_event(UnclassifiedResume, now + Duration::from_secs(40));
        supervisors.user_activity(now + Duration::from_secs(40));
        assert_eq!(supervisors.wake_recovery_until, deadline);
        assert_eq!(supervisors.endpoints[&id].attempts, 10);
    }

    #[test]
    fn unclassified_wake_expedites_a_live_connection_after_its_fresh_probe_fails() {
        use crate::platform::HostPowerEvent::*;
        for healthy in [false, true] {
            let now = Instant::now();
            let profile = profile();
            let id = ClientEndpointId::Ssh(profile.id.clone());
            let mut supervisors = EndpointSupervisors::new(&[profile], now);
            let state = supervisors.endpoints.get_mut(&id).unwrap();
            state.generation = Some(2);
            state.attempts = 9;
            state.next_attempt = None;
            supervisors.host_power_event(Suspend, now);
            supervisors.host_power_event(UnclassifiedResume, now);
            // Only traffic from the current transport consumes its recovery allowance.
            supervisors.received(&id, if healthy { 2 } else { 1 });
            let expired = now + Duration::from_secs(10);
            supervisors.disconnected(&id, 2, expired);
            assert_eq!(
                supervisors.endpoints[&id].next_attempt,
                Some(
                    expired
                        + if healthy {
                            Duration::from_secs(120)
                        } else {
                            Duration::ZERO
                        }
                )
            );
            supervisors.disconnected(&id, 2, expired);
            assert_eq!(
                supervisors.endpoints[&id].next_attempt,
                Some(expired + Duration::from_secs(120))
            );
            assert_eq!(supervisors.endpoints[&id].attempts, 11);
        }
    }

    #[test]
    fn requested_retry_does_not_revive_local_needing_attention() {
        let now = Instant::now();
        let mut supervisors = EndpointSupervisors::new(&[], now);
        supervisors.add_local(PathBuf::from("local.sock"), Some(1), now);
        assert!(supervisors.record_status(
            &ClientEndpointId::Local,
            1,
            ClientEndpointStatus::Attention,
            now
        ));
        assert!(!supervisors.retry_now(&ClientEndpointId::Local, now));
        assert!(supervisors.endpoints[&ClientEndpointId::Local]
            .next_attempt
            .is_none());
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
    fn ssh_recovery_rejects_stale_generations_and_rechecks_attention() {
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
        assert_eq!(
            supervisors.endpoints[&endpoint_id].next_attempt,
            Some(now + Duration::from_secs(30))
        );
    }
}
