//! Non-interactive OpenSSH control transport for remote focus.
//
// This transport opens one lazy SSH-backed wire session per active remote
// focus. It never installs, restarts, upgrades, or hands off a remote server.
#![allow(dead_code)]

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex, Weak};
#[cfg(unix)]
use std::time::{Duration, Instant};

use crate::api::schema::{AgentRef, ErrorBody};
#[cfg(unix)]
use crate::app::remote_focus::RemoteFocusTransition;
#[cfg(unix)]
use crate::pane::ProxyOutbound;
// The transport trait signature names this on every platform; only the Unix
// session loop uses it beyond the signature.
use crate::pane::RemoteProxyChannels;
#[cfg(unix)]
use crate::protocol::{
    self, ClientKeybindings, ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage,
    MAX_FRAME_SIZE, PROTOCOL_VERSION,
};

/// The read half of a split control stream. Owns connection diagnostics:
/// after a read failure the SSH stderr explains the loss better than the I/O
/// error does.
#[cfg(unix)]
pub(crate) trait TimedRead: Read {
    fn read_with_timeout(&mut self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

#[cfg(unix)]
pub(crate) trait ControlReadHalf: TimedRead + Send {
    fn close_diagnostic(&mut self) -> Option<String> {
        None
    }
}

#[cfg(unix)]
pub(crate) trait ControlStream: TimedRead + Write + Send {
    /// Splits the stream after the handshake so the session can read frames
    /// and write input concurrently. The writer half closes the remote
    /// bridge's stdin when dropped.
    fn split(self: Box<Self>) -> (Box<dyn ControlReadHalf>, Box<dyn Write + Send>);

    fn close_diagnostic(&mut self) -> Option<String> {
        None
    }
}

#[cfg(unix)]
pub(crate) trait SshRunner: Send + Sync {
    fn connect(
        &self,
        host: &crate::config::FleetHostConfig,
    ) -> Result<Box<dyn ControlStream>, io::Error>;
}

#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct OpenSshRunner;

#[cfg(unix)]
impl SshRunner for OpenSshRunner {
    fn connect(
        &self,
        host: &crate::config::FleetHostConfig,
    ) -> Result<Box<dyn ControlStream>, io::Error> {
        if host.target.starts_with('-') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH target must not begin with '-'",
            ));
        }
        let mut command = ssh_command(host);
        let mut child = command.spawn()?;
        let Some(stdin) = child.stdin.take() else {
            return Err(io::Error::other("ssh stdin was not available"));
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(io::Error::other("ssh stdout was not available"));
        };
        let stderr = child.stderr.take();
        Ok(Box::new(ProcessControlStream {
            child,
            stdin,
            stdout,
            stderr,
        }))
    }
}

#[cfg(unix)]
fn ssh_command(host: &crate::config::FleetHostConfig) -> std::process::Command {
    let mut command = std::process::Command::new("ssh");
    let remote_command = if let Some(path) = host.socket.as_deref() {
        let session = host
            .session
            .as_deref()
            .map(|name| format!("HERDR_SESSION={} ", shell_quote(name)))
            .unwrap_or_default();
        format!(
            "HERDR_SOCKET_PATH={} {session}herdr remote-control-bridge",
            shell_quote(path)
        )
    } else if let Some(session) = host.session.as_deref() {
        format!(
            "herdr --session {} remote-control-bridge",
            shell_quote(session)
        )
    } else {
        "herdr remote-control-bridge".to_owned()
    };
    command
        .arg("-T")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("RequestTTY=no")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg("--")
        .arg(&host.target)
        .arg(remote_command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn read_with_timeout<R: Read + AsRawFd>(
    reader: &mut R,
    buffer: &mut [u8],
    timeout: Duration,
) -> io::Result<usize> {
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut poll_fd = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
    if ready == 0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for remote control data",
        ));
    }
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    reader.read(buffer)
}

#[cfg(unix)]
struct ProcessControlStream {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    stderr: Option<std::process::ChildStderr>,
}

#[cfg(unix)]
impl Read for ProcessControlStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buffer)
    }
}

#[cfg(unix)]
impl TimedRead for ProcessControlStream {
    fn read_with_timeout(&mut self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize> {
        read_with_timeout(&mut self.stdout, buffer, timeout)
    }
}

#[cfg(unix)]
impl Write for ProcessControlStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stdin.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

#[cfg(unix)]
impl ControlStream for ProcessControlStream {
    fn split(self: Box<Self>) -> (Box<dyn ControlReadHalf>, Box<dyn Write + Send>) {
        let this = *self;
        (
            Box::new(ProcessControlReader {
                child: this.child,
                stdout: this.stdout,
                stderr: this.stderr,
            }),
            Box::new(this.stdin),
        )
    }

    fn close_diagnostic(&mut self) -> Option<String> {
        let _ = self.child.kill();
        let mut output = Vec::new();
        if let Some(stderr) = &mut self.stderr {
            let _ = stderr.read_to_end(&mut output);
        }
        let _ = self.child.wait();
        (!output.is_empty()).then(|| String::from_utf8_lossy(&output).into_owned())
    }
}

#[cfg(unix)]
struct ProcessControlReader {
    child: std::process::Child,
    stdout: std::process::ChildStdout,
    stderr: Option<std::process::ChildStderr>,
}

#[cfg(unix)]
impl Read for ProcessControlReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buffer)
    }
}

#[cfg(unix)]
impl TimedRead for ProcessControlReader {
    fn read_with_timeout(&mut self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize> {
        read_with_timeout(&mut self.stdout, buffer, timeout)
    }
}

#[cfg(unix)]
impl ControlReadHalf for ProcessControlReader {
    fn close_diagnostic(&mut self) -> Option<String> {
        let _ = self.child.kill();
        let mut output = Vec::new();
        if let Some(stderr) = &mut self.stderr {
            let _ = stderr.read_to_end(&mut output);
        }
        let _ = self.child.wait();
        (!output.is_empty()).then(|| String::from_utf8_lossy(&output).into_owned())
    }
}

#[cfg(unix)]
impl Drop for ProcessControlReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Shared, idempotent termination for one control session.
#[cfg(unix)]
struct ControlSessionTermination {
    sessions: Weak<Mutex<std::collections::HashMap<String, SessionHandle>>>,
    operation_id: String,
    detached: Arc<AtomicBool>,
    input_enabled: Arc<AtomicBool>,
    operation_state: Arc<crate::remote::RemoteFocusOperationState>,
}

#[cfg(unix)]
impl ControlSessionTermination {
    fn terminate(&self) {
        self.operation_state.terminate();
        if let Some(sessions) = self.sessions.upgrade() {
            finish_control_session(
                &sessions,
                &self.operation_id,
                &self.detached,
                &self.input_enabled,
            );
        } else {
            close_input_gate_and_detach(&self.input_enabled, &self.detached);
        }
    }
}

/// One live control session. `detached` distinguishes an intentional local
/// detach (quiet close) from a connection loss (ambiguous delivery).
#[cfg(unix)]
#[derive(Clone)]
struct SessionHandle {
    host: String,
    outbound_tx: tokio::sync::mpsc::Sender<ProxyOutbound>,
    detach_requested: Arc<AtomicBool>,
    writer_decision: Arc<Mutex<()>>,
    termination: Arc<ControlSessionTermination>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteHostIdentity {
    remote_name: String,
    target: String,
    socket: Option<String>,
    session: Option<String>,
}

pub(crate) struct SshRemoteFocusTransport {
    #[cfg(unix)]
    runner: Arc<dyn SshRunner>,
    #[cfg(unix)]
    sessions: Arc<Mutex<std::collections::HashMap<String, SessionHandle>>>,
    #[cfg(all(test, unix))]
    writer_start_gate: Option<Arc<std::sync::Barrier>>,
    hosts: std::collections::HashMap<String, crate::config::FleetHostConfig>,
    remote_identities: std::collections::HashMap<String, RemoteHostIdentity>,
    config_generation: u64,
}

impl SshRemoteFocusTransport {
    pub(crate) fn new(fleet: &crate::config::FleetConfig) -> Self {
        let hosts = Self::admitted_hosts(fleet);
        Self {
            #[cfg(unix)]
            runner: Arc::new(OpenSshRunner),
            #[cfg(unix)]
            sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
            #[cfg(all(test, unix))]
            writer_start_gate: None,
            hosts,
            remote_identities: std::collections::HashMap::new(),
            config_generation: 0,
        }
    }

    fn admitted_hosts(
        fleet: &crate::config::FleetConfig,
    ) -> std::collections::HashMap<String, crate::config::FleetHostConfig> {
        crate::fleet::select_hosts(fleet, None)
            .unwrap_or_default()
            .into_iter()
            .filter(|host| !host.local && !host.target.trim().is_empty())
            .map(|host| (host.name.clone(), host))
            .collect()
    }

    #[cfg(all(test, unix))]
    pub(crate) fn with_runner(
        fleet: &crate::config::FleetConfig,
        runner: Arc<dyn SshRunner>,
    ) -> Self {
        let mut transport = Self::new(fleet);
        transport.runner = runner;
        transport
    }

    #[cfg(unix)]
    fn reconfigure(&mut self, fleet: &crate::config::FleetConfig) -> Vec<String> {
        let next_hosts = Self::admitted_hosts(fleet);
        let revoked = {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions
                .iter()
                .filter_map(|(operation_id, handle)| {
                    let changed = match (next_hosts.get(&handle.host), self.hosts.get(&handle.host))
                    {
                        (Some(next), Some(previous)) => {
                            next.target != previous.target
                                || next.socket != previous.socket
                                || next.session != previous.session
                        }
                        (None, Some(_)) | (Some(_), None) => true,
                        (None, None) => false,
                    };
                    changed.then(|| (operation_id.clone(), handle.clone()))
                })
                .collect::<Vec<_>>()
        };
        for (_, handle) in &revoked {
            {
                let _decision = handle
                    .writer_decision
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                handle.detach_requested.store(true, Ordering::Release);
            }
            handle.termination.terminate();
            let _ = handle.outbound_tx.try_send(ProxyOutbound::Detach);
        }
        self.remote_identities.retain(|name, identity| {
            next_hosts
                .get(name)
                .is_some_and(|host| host_matches_identity(host, identity))
        });
        self.hosts = next_hosts;
        // Advancing the generation rejects every in-flight observation, even
        // when its connection tuple still matches an unchanged host.
        self.config_generation = self.config_generation.saturating_add(1);
        revoked
            .into_iter()
            .map(|(operation_id, _)| operation_id)
            .collect()
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    fn start_with_options(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        version: u32,
        build_version: String,
        expected_context: Option<Box<crate::api::schema::RemoteControlContext>>,
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        let Some(host) = self.hosts.get(&agent_ref.host).cloned() else {
            return Err(ErrorBody {
                code: "host_unreachable".to_owned(),
                message: format!("remote host alias {} is not configured", agent_ref.host),
            });
        };
        let Some(identity) = self.remote_identities.get(&agent_ref.host).cloned() else {
            return Err(ErrorBody {
                code: "host_unreachable".to_owned(),
                message: format!(
                    "remote host {} has no unambiguous identity from a successful fleet poll",
                    agent_ref.host
                ),
            });
        };
        if !host_matches_identity(&host, &identity) {
            return Err(ErrorBody {
                code: "host_unreachable".to_owned(),
                message: format!(
                    "remote host {} identity is stale for its configured connection",
                    agent_ref.host
                ),
            });
        }
        let configured_host = agent_ref.host.clone();
        let remote_host = identity.remote_name.clone();
        let wire_agent_ref =
            AgentRef::new(remote_host.clone(), agent_ref.agent.clone()).map_err(|_| ErrorBody {
                code: "host_unreachable".to_owned(),
                message: format!("remote host identity is invalid for {}", agent_ref.host),
            })?;
        let expected_context = expected_context
            .map(|context| {
                translate_context_host(
                    &context,
                    &[configured_host.as_str(), remote_host.as_str()],
                    &remote_host,
                )
            })
            .transpose()?;
        let operation_id = operation_id.to_owned();
        let agent_ref = wire_agent_ref;
        let runner = Arc::clone(&self.runner);
        let detached = Arc::new(AtomicBool::new(false));
        let detach_requested = Arc::new(AtomicBool::new(false));
        let writer_decision = Arc::new(Mutex::new(()));
        let input_enabled = Arc::clone(&channels.input_enabled);
        let sessions = Arc::clone(&self.sessions);
        let termination = Arc::new(ControlSessionTermination {
            sessions: Arc::downgrade(&sessions),
            operation_id: operation_id.clone(),
            detached: Arc::clone(&detached),
            input_enabled: Arc::clone(&input_enabled),
            operation_state: Arc::clone(&channels.operation_state),
        });
        let failure_reported = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let writer_start_gate = self.writer_start_gate.clone();
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                operation_id.clone(),
                SessionHandle {
                    host: configured_host.clone(),
                    outbound_tx: channels.detach_tx.clone(),
                    detach_requested: Arc::clone(&detach_requested),
                    writer_decision: Arc::clone(&writer_decision),
                    termination: Arc::clone(&termination),
                },
            );
        let cleanup = ControlSessionTeardown::new(Arc::clone(&termination));
        let spawn_result = std::thread::Builder::new()
            .name(format!("herdr-remote-focus-{operation_id}"))
            .spawn(move || {
                // The guard owns the only teardown path. It is constructed
                // before spawn so dropping a failed spawn also unregisters
                // the session; closure unwinding covers panic/cancellation.
                let cleanup_detached = Arc::clone(&cleanup.termination.detached);
                let panic_operation_id = cleanup.termination.operation_id.clone();
                let cleanup_event_tx = event_tx.clone();
                let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_control_session(
                        runner,
                        host,
                        operation_id,
                        agent_ref,
                        configured_host,
                        remote_host,
                        version,
                        build_version,
                        expected_context,
                        channels,
                        cleanup_detached,
                        detach_requested,
                        writer_decision,
                        failure_reported,
                        Arc::clone(&cleanup.termination),
                        #[cfg(test)]
                        writer_start_gate,
                        event_tx,
                    )
                }));
                if let Err(payload) = panicked {
                    drop(cleanup);
                    SshRemoteFocusTransport::fail(
                        &cleanup_event_tx,
                        &panic_operation_id,
                        "connection_lost",
                        "remote control session panicked; delivery of the last accepted batch is unknown",
                    );
                    std::panic::resume_unwind(payload);
                }
            });
        if let Err(error) = spawn_result {
            return Err(ErrorBody {
                code: "host_unreachable".to_owned(),
                message: format!("failed to start remote focus connection: {error}"),
            });
        }
        Ok(())
    }

    #[cfg(all(test, unix))]
    pub(crate) fn start_with_expected_context_and_version_for_test(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        expected_context: Option<crate::api::schema::RemoteControlContext>,
        version: u32,
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        self.start_with_options(
            operation_id,
            agent_ref,
            version,
            crate::build_info::version().to_owned(),
            expected_context.map(Box::new),
            channels,
            event_tx,
        )
    }

    #[cfg(unix)]
    fn fail(
        event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        operation_id: &str,
        code: &str,
        message: impl Into<String>,
    ) {
        let _ = event_tx.blocking_send(crate::events::AppEvent::RemoteFocusTransition {
            operation_id: operation_id.to_owned(),
            transition: Box::new(RemoteFocusTransition::Failed(ErrorBody {
                code: code.to_owned(),
                message: message.into(),
            })),
        });
    }

    #[cfg(unix)]
    fn fail_once(
        failure_reported: &AtomicBool,
        event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        operation_id: &str,
        code: &str,
        message: impl Into<String>,
    ) {
        if !failure_reported.swap(true, Ordering::AcqRel) {
            Self::fail(event_tx, operation_id, code, message);
        }
    }
}

impl std::fmt::Debug for SshRemoteFocusTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SshRemoteFocusTransport")
            .field("hosts", &self.hosts)
            .finish_non_exhaustive()
    }
}

impl crate::app::remote_focus::RemoteFocusTransport for SshRemoteFocusTransport {
    fn accepts_remote_host(&self, host: &str) -> bool {
        self.hosts.contains_key(host)
    }

    fn remote_host_ready(&self, host: &str) -> bool {
        self.hosts
            .get(host)
            .and_then(|configured| {
                self.remote_identities
                    .get(host)
                    .filter(|identity| host_matches_identity(configured, identity))
            })
            .is_some()
    }

    fn reload_fleet(&mut self, fleet: &crate::config::FleetConfig) -> Vec<String> {
        #[cfg(unix)]
        {
            self.reconfigure(fleet)
        }
        #[cfg(not(unix))]
        {
            let next_hosts = Self::admitted_hosts(fleet);
            self.remote_identities.retain(|name, identity| {
                next_hosts
                    .get(name)
                    .is_some_and(|host| host_matches_identity(host, identity))
            });
            self.hosts = next_hosts;
            self.config_generation = self.config_generation.saturating_add(1);
            Vec::new()
        }
    }

    fn observe_fleet_snapshot(&mut self, snapshot: &crate::fleet::Snapshot) {
        if snapshot.config_generation != self.config_generation {
            return;
        }
        let mut observations = std::collections::HashMap::with_capacity(snapshot.hosts.len());
        for observed in &snapshot.hosts {
            match observations.entry(observed.name.as_str()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Some(observed));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
        }
        for (name, configured) in &self.hosts {
            let Some(Some(observed)) = observations.get(name.as_str()) else {
                self.remote_identities.remove(name);
                continue;
            };
            let Some(remote_name) = observed_remote_identity(configured, observed) else {
                self.remote_identities.remove(name);
                continue;
            };
            self.remote_identities.insert(
                name.clone(),
                RemoteHostIdentity {
                    remote_name: remote_name.to_owned(),
                    target: observed.target.clone(),
                    socket: observed.socket.clone(),
                    session: observed.session.clone(),
                },
            );
        }
    }

    fn start(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        _proxy_pane_id: &str,
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        #[cfg(not(unix))]
        {
            let _ = (operation_id, agent_ref, channels, event_tx);
            Err(ErrorBody {
                code: "agent_not_attachable".to_owned(),
                message: "remote focus requires a Unix client and Unix server".to_owned(),
            })
        }
        #[cfg(unix)]
        {
            self.start_with_options(
                operation_id,
                agent_ref,
                PROTOCOL_VERSION,
                crate::build_info::version().to_owned(),
                None,
                channels,
                event_tx,
            )
        }
    }

    fn detach(&mut self, operation_id: &str) {
        #[cfg(unix)]
        {
            let handle = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(operation_id)
                .cloned();
            let Some(handle) = handle else {
                return;
            };
            {
                let _decision = handle
                    .writer_decision
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                handle.detach_requested.store(true, Ordering::Release);
            }
            handle.termination.terminate();
            // The flag is the urgent side channel. If the normal queue is
            // full, the writer drops queued input and writes Detach directly
            // instead of waiting behind it.
            let _ = handle.outbound_tx.try_send(ProxyOutbound::Detach);
        }
        #[cfg(not(unix))]
        {
            let _ = operation_id;
        }
    }
}

fn observed_remote_identity<'a>(
    configured: &crate::config::FleetHostConfig,
    observed: &'a crate::fleet::HostSnapshot,
) -> Option<&'a str> {
    if configured.local
        || observed.local
        || observed.state != crate::fleet::HostState::Reachable
        || configured.target != observed.target
        || configured.socket != observed.socket
        || configured.session != observed.session
    {
        return None;
    }
    observed
        .remote_identity
        .as_deref()
        .filter(|remote_name| !remote_name.is_empty() && !remote_name.contains("::"))
}

fn host_matches_identity(
    host: &crate::config::FleetHostConfig,
    identity: &RemoteHostIdentity,
) -> bool {
    !host.local
        && host.target == identity.target
        && host.socket == identity.socket
        && host.session == identity.session
}

#[cfg(unix)]
fn translate_context_host(
    context: &crate::api::schema::RemoteControlContext,
    accepted_hosts: &[&str],
    target_host: &str,
) -> Result<Box<crate::api::schema::RemoteControlContext>, ErrorBody> {
    if !accepted_hosts.contains(&context.host.as_str()) {
        return Err(ErrorBody {
            code: "refused_for_safety".to_owned(),
            message: "remote control context host does not match the expected host".to_owned(),
        });
    }
    let mut translated = context.clone();
    translated.host = target_host.to_owned();
    Ok(Box::new(translated))
}

#[cfg(unix)]
const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(unix)]
struct DeadlineReader<'a> {
    reader: &'a mut dyn TimedRead,
    deadline: Instant,
}

#[cfg(unix)]
impl Read for DeadlineReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let Some(timeout) = self.deadline.checked_duration_since(Instant::now()) else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "remote control handshake timed out",
            ));
        };
        self.reader.read_with_timeout(buffer, timeout)
    }
}

#[cfg(unix)]
fn read_frame_with_deadline(
    reader: &mut dyn TimedRead,
    deadline: Instant,
) -> Result<Vec<u8>, protocol::FramingError> {
    let mut reader = DeadlineReader { reader, deadline };
    protocol::read_frame(&mut reader, MAX_FRAME_SIZE)
}

#[cfg(unix)]
fn read_message_with_deadline(
    reader: &mut dyn TimedRead,
    deadline: Instant,
) -> Result<ServerMessage, protocol::FramingError> {
    let mut reader = DeadlineReader { reader, deadline };
    protocol::read_message(&mut reader, MAX_FRAME_SIZE)
}

#[cfg(unix)]
fn read_initial_welcome(
    stream: &mut dyn ControlStream,
    deadline: Instant,
) -> Result<ServerMessage, protocol::FramingError> {
    let payload = read_frame_with_deadline(stream, deadline)?;
    protocol::decode_frame(&payload)
        .or_else(|error| protocol::decode_legacy_server_welcome(&payload).ok_or(error))
}

#[cfg(unix)]
struct CancellableReader<'a> {
    reader: &'a mut dyn ControlReadHalf,
    detached: &'a AtomicBool,
}

#[cfg(unix)]
impl Read for CancellableReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.detached.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "remote control session detached",
            ));
        }
        loop {
            match self
                .reader
                .read_with_timeout(buffer, Duration::from_millis(100))
            {
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    if self.detached.load(Ordering::Acquire) {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "remote control session detached",
                        ));
                    }
                }
                Ok(read) if self.detached.load(Ordering::Acquire) => {
                    let _ = read;
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "remote control session detached",
                    ));
                }
                result => return result,
            }
        }
    }
}

/// Ends the session: marks it detached so a late local detach is a no-op and
/// unregisters it. The writer exits on its own: a queued detach, a write
/// failure, or the channel closing once the app and this handle drop their
/// senders. Nothing more is written here — after a connection loss no bytes
/// may reach the wire.
#[cfg(unix)]
fn close_input_gate_and_detach(input_enabled: &AtomicBool, detached: &AtomicBool) {
    input_enabled.store(false, Ordering::Release);
    detached.store(true, Ordering::Release);
}

#[cfg(unix)]
fn finish_control_session(
    sessions: &Mutex<std::collections::HashMap<String, SessionHandle>>,
    operation_id: &str,
    detached: &Arc<AtomicBool>,
    input_enabled: &Arc<AtomicBool>,
) {
    close_input_gate_and_detach(input_enabled, detached);
    let mut sessions = sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if sessions
        .get(operation_id)
        .is_some_and(|handle| Arc::ptr_eq(&handle.termination.detached, detached))
    {
        sessions.remove(operation_id);
    }
}

#[cfg(unix)]
struct ControlSessionTeardown {
    termination: Arc<ControlSessionTermination>,
}

#[cfg(unix)]
impl ControlSessionTeardown {
    fn new(termination: Arc<ControlSessionTermination>) -> Self {
        Self { termination }
    }
}

#[cfg(unix)]
impl Drop for ControlSessionTeardown {
    fn drop(&mut self) {
        self.termination.terminate();
    }
}

#[cfg(unix)]
fn read_resize_slot(resize_slot: &Mutex<(u16, u16, u32, u32)>) -> (u16, u16, u32, u32) {
    match resize_slot.lock() {
        Ok(slot) => *slot,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// Forwards proxy outbound messages to the wire. After handling each item, the
/// writer compares the latest slot with the last geometry it wrote and emits a
/// resize when they differ. A resize marker only wakes this check. Ends on
/// detach, on a closed channel (best-effort detach so the remote lease is
/// still released), or on a write failure (the writer reports the loss and
/// closes the local input gate).
#[cfg(unix)]
fn run_control_writer(
    mut writer: Box<dyn Write + Send>,
    mut outbound_rx: tokio::sync::mpsc::Receiver<ProxyOutbound>,
    resize_slot: Arc<Mutex<(u16, u16, u32, u32)>>,
    initial_resize: Option<(u16, u16, u32, u32)>,
    detached: Arc<AtomicBool>,
    detach_requested: Arc<AtomicBool>,
    writer_decision: Arc<Mutex<()>>,
    failure_reported: Arc<AtomicBool>,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    operation_id: String,
    termination: Arc<ControlSessionTermination>,
) {
    let mut last_sent_resize = initial_resize;
    while let Some(message) = outbound_rx.blocking_recv() {
        let is_input = matches!(message, ProxyOutbound::Input(_));
        let is_detach = matches!(message, ProxyOutbound::Detach);
        let wire = match message {
            ProxyOutbound::Input(bytes) => Some(ClientMessage::Input {
                data: bytes.to_vec(),
            }),
            ProxyOutbound::SyncResize => None,
            ProxyOutbound::Detach => Some(ClientMessage::Detach),
        };
        if let Some(wire) = wire {
            match write_control_message(
                &mut writer,
                &wire,
                is_input,
                is_detach,
                &detached,
                &detach_requested,
                &writer_decision,
                &termination.operation_state,
            ) {
                WriterWriteOutcome::Skipped => return,
                WriterWriteOutcome::Sent { stop } if stop => return,
                WriterWriteOutcome::Sent { stop: _ } => {}
                WriterWriteOutcome::Failed {
                    detail,
                    intentional,
                } => {
                    termination.terminate();
                    if !intentional {
                        SshRemoteFocusTransport::fail_once(
                            &failure_reported,
                            &event_tx,
                            &operation_id,
                            "connection_lost",
                            format!(
                                "remote control writer failed; delivery of the last accepted batch is unknown: {detail}"
                            ),
                        );
                    } else {
                        detached.store(true, Ordering::Release);
                    }
                    return;
                }
            }
        }
        if is_detach {
            return;
        }
        let resize = read_resize_slot(&resize_slot);
        if last_sent_resize != Some(resize) {
            let resize_message = ClientMessage::Resize {
                cols: resize.1,
                rows: resize.0,
                cell_width_px: resize.2,
                cell_height_px: resize.3,
            };
            match write_control_message(
                &mut writer,
                &resize_message,
                false,
                false,
                &detached,
                &detach_requested,
                &writer_decision,
                &termination.operation_state,
            ) {
                WriterWriteOutcome::Skipped => return,
                WriterWriteOutcome::Sent { stop } if stop => return,
                WriterWriteOutcome::Sent { stop: _ } => {}
                WriterWriteOutcome::Failed {
                    detail,
                    intentional,
                } => {
                    termination.terminate();
                    if !intentional {
                        SshRemoteFocusTransport::fail_once(
                            &failure_reported,
                            &event_tx,
                            &operation_id,
                            "connection_lost",
                            format!(
                                "remote control writer failed; delivery of the last accepted batch is unknown: {detail}"
                            ),
                        );
                    }
                    return;
                }
            }
            last_sent_resize = Some(resize);
        }
    }
    match write_control_message(
        &mut writer,
        &ClientMessage::Detach,
        false,
        true,
        &detached,
        &detach_requested,
        &writer_decision,
        &termination.operation_state,
    ) {
        WriterWriteOutcome::Failed {
            detail,
            intentional,
        } => {
            termination.terminate();
            if !intentional {
                SshRemoteFocusTransport::fail_once(
                    &failure_reported,
                    &event_tx,
                    &operation_id,
                    "connection_lost",
                    format!(
                        "remote control writer failed; delivery of the last accepted batch is unknown: {detail}"
                    ),
                );
            }
        }
        WriterWriteOutcome::Skipped | WriterWriteOutcome::Sent { .. } => {}
    }
}

#[cfg(unix)]
enum WriterWriteOutcome {
    Skipped,
    Sent { stop: bool },
    Failed { detail: String, intentional: bool },
}

#[cfg(unix)]
fn write_control_message(
    writer: &mut Box<dyn Write + Send>,
    message: &ClientMessage,
    is_input: bool,
    stop: bool,
    detached: &AtomicBool,
    detach_requested: &AtomicBool,
    writer_decision: &Mutex<()>,
    operation_state: &crate::remote::RemoteFocusOperationState,
) -> WriterWriteOutcome {
    let detach_message = ClientMessage::Detach;
    let (message, stop, intentional) = {
        let _decision = writer_decision
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if detach_requested.load(Ordering::Acquire) {
            (&detach_message, true, true)
        } else if detached.load(Ordering::Acquire) {
            return WriterWriteOutcome::Skipped;
        } else {
            if is_input {
                operation_state.before_input_send();
                let admission = operation_state.lock_input_admission();
                if operation_state.is_terminal() {
                    return WriterWriteOutcome::Skipped;
                }
                drop(admission);
            }
            (message, stop, false)
        }
    };
    // The decision is serialized with detach, but wire I/O must not be: SSH
    // stdin can block indefinitely while the app still needs to close.
    match protocol::write_message(writer, message) {
        Ok(()) => WriterWriteOutcome::Sent { stop },
        Err(error) => WriterWriteOutcome::Failed {
            detail: error.to_string(),
            intentional,
        },
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_control_session(
    runner: Arc<dyn SshRunner>,
    host: crate::config::FleetHostConfig,
    operation_id: String,
    agent_ref: AgentRef,
    configured_host: String,
    remote_host: String,
    version: u32,
    build_version: String,
    expected_context: Option<Box<crate::api::schema::RemoteControlContext>>,
    channels: RemoteProxyChannels,
    detached: Arc<AtomicBool>,
    detach_requested: Arc<AtomicBool>,
    writer_decision: Arc<Mutex<()>>,
    failure_reported: Arc<AtomicBool>,
    termination: Arc<ControlSessionTermination>,
    #[cfg(test)] writer_start_gate: Option<Arc<std::sync::Barrier>>,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    let RemoteProxyChannels {
        outbound_rx,
        detach_tx,
        resize_slot,
        ..
    } = channels;
    if detached.load(Ordering::Acquire) {
        return;
    }
    let mut stream = match runner.connect(&host) {
        Ok(stream) => stream,
        Err(error) => {
            termination.terminate();
            SshRemoteFocusTransport::fail_once(
                &failure_reported,
                &event_tx,
                &operation_id,
                "host_unreachable",
                error.to_string(),
            );
            return;
        }
    };
    let (hello_rows, hello_cols, hello_cell_width_px, hello_cell_height_px) =
        read_resize_slot(&resize_slot);
    let hello = ClientMessage::Hello {
        version,
        build_version,
        cols: hello_cols,
        rows: hello_rows,
        cell_width_px: hello_cell_width_px,
        cell_height_px: hello_cell_height_px,
        requested_encoding: RenderEncoding::TerminalAnsi,
        keybindings: ClientKeybindings::Server,
        launch_mode: ClientLaunchMode::TerminalAttach,
    };
    if let Err(error) = protocol::write_message(&mut stream, &hello) {
        let diagnostic = stream.close_diagnostic();
        let detail = diagnostic
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| error.to_string());
        termination.terminate();
        SshRemoteFocusTransport::fail_once(
            &failure_reported,
            &event_tx,
            &operation_id,
            "host_unreachable",
            format!("remote control handshake write failed: {detail}"),
        );
        return;
    }
    let welcome: ServerMessage =
        match read_initial_welcome(&mut *stream, Instant::now() + CONTROL_HANDSHAKE_TIMEOUT) {
            Ok(message) => message,
            Err(error) => {
                let diagnostic = stream.close_diagnostic();
                let detail = diagnostic
                    .filter(|message| !message.trim().is_empty())
                    .unwrap_or_else(|| error.to_string());
                termination.terminate();
                SshRemoteFocusTransport::fail_once(
                    &failure_reported,
                    &event_tx,
                    &operation_id,
                    "host_unreachable",
                    format!("remote control handshake read failed: {detail}"),
                );
                return;
            }
        };
    let ServerMessage::Welcome {
        version,
        build_version,
        error,
        ..
    } = welcome
    else {
        termination.terminate();
        SshRemoteFocusTransport::fail_once(
            &failure_reported,
            &event_tx,
            &operation_id,
            "version_skew",
            "remote control did not receive a Welcome message",
        );
        return;
    };
    if error.is_some()
        || version != PROTOCOL_VERSION
        || build_version.is_empty()
        || build_version != crate::build_info::version()
    {
        termination.terminate();
        SshRemoteFocusTransport::fail_once(
            &failure_reported,
            &event_tx,
            &operation_id,
            "version_skew",
            format!(
                "remote control requires protocol {} and build {}, got protocol {} and build {}{}",
                PROTOCOL_VERSION,
                crate::build_info::version(),
                version,
                if build_version.is_empty() {
                    "<missing>"
                } else {
                    &build_version
                },
                error
                    .as_deref()
                    .map_or(String::new(), |error| format!(" ({error})")),
            ),
        );
        return;
    }
    {
        // Serialize the decision with reload revocation, but not the wire write:
        // SSH stdin can block indefinitely while the app still needs to close.
        let _decision = writer_decision
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if detached.load(Ordering::Acquire) || detach_requested.load(Ordering::Acquire) {
            return;
        }
    }
    let control_write = protocol::write_message(
        &mut stream,
        &ClientMessage::ControlTerminal {
            target: agent_ref.to_string(),
            agent_ref: Some(agent_ref),
            expected_context,
            takeover: false,
        },
    );
    if let Err(error) = control_write {
        termination.terminate();
        SshRemoteFocusTransport::fail_once(
            &failure_reported,
            &event_tx,
            &operation_id,
            "host_unreachable",
            format!("remote control request write failed: {error}"),
        );
        return;
    }
    let (mut reader, writer) = stream.split();
    let writer_detached = Arc::clone(&detached);
    let writer_detach_requested = Arc::clone(&detach_requested);
    let writer_decision = Arc::clone(&writer_decision);
    let writer_resize_slot = Arc::clone(&resize_slot);
    let writer_failure_reported = Arc::clone(&failure_reported);
    let writer_event_tx = event_tx.clone();
    let writer_operation_id = operation_id.clone();
    let writer_termination = Arc::clone(&termination);
    let writer_initial_resize = Some((
        hello_rows,
        hello_cols,
        hello_cell_width_px,
        hello_cell_height_px,
    ));
    let writer_thread = std::thread::Builder::new()
        .name(format!("herdr-remote-focus-writer-{operation_id}"))
        .spawn(move || {
            #[cfg(test)]
            if let Some(gate) = writer_start_gate {
                gate.wait();
            }
            run_control_writer(
                writer,
                outbound_rx,
                writer_resize_slot,
                writer_initial_resize,
                writer_detached,
                writer_detach_requested,
                writer_decision,
                writer_failure_reported,
                writer_event_tx,
                writer_operation_id,
                writer_termination,
            )
        });
    if let Err(error) = writer_thread {
        termination.terminate();
        SshRemoteFocusTransport::fail_once(
            &failure_reported,
            &event_tx,
            &operation_id,
            "host_unreachable",
            format!("failed to start remote focus writer: {error}"),
        );
        return;
    }
    let mut active = false;
    let control_ready_deadline = Instant::now() + CONTROL_HANDSHAKE_TIMEOUT;
    loop {
        if detached.load(Ordering::Acquire) {
            break;
        }
        let message: ServerMessage = match if active {
            let mut reader = CancellableReader {
                reader: &mut *reader,
                detached: &detached,
            };
            protocol::read_message(&mut reader, MAX_FRAME_SIZE)
        } else {
            read_message_with_deadline(&mut *reader, control_ready_deadline)
        } {
            Ok(_message) if detached.load(Ordering::Acquire) => break,
            Ok(message) => message,
            Err(error) => {
                if detached.load(Ordering::Acquire) {
                    break;
                }
                termination.terminate();
                let _ = detach_tx.try_send(ProxyOutbound::Detach);
                let diagnostic = reader.close_diagnostic();
                let detail = diagnostic
                    .filter(|message| !message.trim().is_empty())
                    .unwrap_or_else(|| error.to_string());
                SshRemoteFocusTransport::fail_once(
                    &failure_reported,
                    &event_tx,
                    &operation_id,
                    if active {
                        "connection_lost"
                    } else {
                        "host_unreachable"
                    },
                    if active {
                        format!("remote control connection lost; delivery of the last accepted batch is unknown: {detail}")
                    } else {
                        format!("remote control request read failed: {detail}")
                    },
                );
                break;
            }
        };
        if detached.load(Ordering::Acquire) {
            break;
        }
        match message {
            ServerMessage::ControlReady { context } => {
                let context = match translate_context_host(
                    &context,
                    &[remote_host.as_str()],
                    &configured_host,
                ) {
                    Ok(context) => context,
                    Err(error) => {
                        termination.terminate();
                        let _ = detach_tx.try_send(ProxyOutbound::Detach);
                        SshRemoteFocusTransport::fail_once(
                            &failure_reported,
                            &event_tx,
                            &operation_id,
                            &error.code,
                            error.message,
                        );
                        break;
                    }
                };
                active = true;
                if event_tx
                    .blocking_send(crate::events::AppEvent::RemoteFocusTransition {
                        operation_id: operation_id.clone(),
                        transition: Box::new(RemoteFocusTransition::Active(context)),
                    })
                    .is_err()
                {
                    termination.terminate();
                    let _ = detach_tx.try_send(ProxyOutbound::Detach);
                    break;
                }
            }
            ServerMessage::Terminal(frame) => {
                // Frames are an ordered diff stream: block on a full event
                // channel rather than drop one and corrupt later diffs.
                if event_tx
                    .blocking_send(crate::events::AppEvent::RemoteFocusFrame {
                        operation_id: operation_id.clone(),
                        frame: Box::new(frame),
                    })
                    .is_err()
                {
                    termination.terminate();
                    let _ = detach_tx.try_send(ProxyOutbound::Detach);
                    break;
                }
            }
            ServerMessage::ControlError { code, message } => {
                if !detached.load(Ordering::Acquire) {
                    termination.terminate();
                    let _ = detach_tx.try_send(ProxyOutbound::Detach);
                    SshRemoteFocusTransport::fail_once(
                        &failure_reported,
                        &event_tx,
                        &operation_id,
                        &code,
                        message,
                    );
                }
                break;
            }
            ServerMessage::ServerShutdown { reason } => {
                if !detached.load(Ordering::Acquire) {
                    termination.terminate();
                    let _ = detach_tx.try_send(ProxyOutbound::Detach);
                    SshRemoteFocusTransport::fail_once(
                        &failure_reported,
                        &event_tx,
                        &operation_id,
                        if active {
                            "connection_lost"
                        } else {
                            "host_unreachable"
                        },
                        reason.unwrap_or_else(|| {
                            "remote control server closed the connection".to_owned()
                        }),
                    );
                }
                break;
            }
            ServerMessage::Graphics { .. } => {}
            _ => {}
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::remote_focus::RemoteFocusTransport;
    use std::io::Cursor;
    use std::sync::Mutex;

    struct FakeStream {
        input: Cursor<Vec<u8>>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
        write_error: Option<String>,
        write_gate: Option<Arc<WriteGate>>,
        diagnostic: Option<String>,
        handshake_write_gate: Option<HandshakeWriteGate>,
    }

    /// Stalls one pre-split stream write so a test can revoke mid-request.
    struct HandshakeWriteGate {
        stalled_write: usize,
        next_write: usize,
        started_tx: std::sync::mpsc::Sender<()>,
        release_rx: std::sync::mpsc::Receiver<()>,
    }

    impl Read for FakeStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            // The handshake reads through the whole stream before the split;
            // the gate only holds the post-split reader.
            read_fake_input(&mut self.input, self.fail_at, None, None, buffer)
        }
    }

    impl TimedRead for FakeStream {
        fn read_with_timeout(&mut self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize> {
            if self
                .read_gate
                .as_ref()
                .is_some_and(|gate| !gate.load(Ordering::Acquire))
                && self.input.position() >= self.input.get_ref().len() as u64
            {
                std::thread::sleep(timeout.min(Duration::from_millis(5)));
                return Err(io::Error::new(io::ErrorKind::TimedOut, "fake read timeout"));
            }
            self.read(buffer)
        }
    }

    impl Write for FakeStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if let Some(gate) = &mut self.handshake_write_gate {
                let write_index = gate.next_write;
                gate.next_write += 1;
                if write_index == gate.stalled_write {
                    let _ = gate.started_tx.send(());
                    let _ = gate.release_rx.recv();
                }
            }
            self.output
                .lock()
                .expect("fake output lock")
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl ControlStream for FakeStream {
        fn split(self: Box<Self>) -> (Box<dyn ControlReadHalf>, Box<dyn Write + Send>) {
            let this = *self;
            (
                Box::new(FakeReader {
                    input: this.input,
                    fail_at: this.fail_at,
                    panic_at: this.panic_at,
                    read_gate: this.read_gate,
                }),
                Box::new(FakeWriter {
                    output: this.output,
                    write_error: this.write_error,
                    write_gate: this.write_gate,
                }),
            )
        }

        fn close_diagnostic(&mut self) -> Option<String> {
            self.diagnostic.take()
        }
    }

    fn read_fake_input(
        input: &mut Cursor<Vec<u8>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<&AtomicBool>,
        buffer: &mut [u8],
    ) -> io::Result<usize> {
        // A closed gate holds the reader only once the scripted bytes are
        // consumed, so a test can order a detach before the stream ends
        // without blocking scripted messages.
        while read_gate.is_some_and(|gate| !gate.load(Ordering::Acquire))
            && input.position() >= input.get_ref().len() as u64
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if panic_at.is_some_and(|offset| input.position() >= offset) {
            panic!("injected session panic");
        }
        if fail_at.is_some_and(|offset| input.position() >= offset) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "mid-stream failure",
            ));
        }
        input.read(buffer)
    }

    struct FakeReader {
        input: Cursor<Vec<u8>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
    }

    impl Read for FakeReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            read_fake_input(
                &mut self.input,
                self.fail_at,
                self.panic_at,
                self.read_gate.as_deref(),
                buffer,
            )
        }
    }

    impl TimedRead for FakeReader {
        fn read_with_timeout(&mut self, buffer: &mut [u8], timeout: Duration) -> io::Result<usize> {
            if self
                .read_gate
                .as_ref()
                .is_some_and(|gate| !gate.load(Ordering::Acquire))
                && self.input.position() >= self.input.get_ref().len() as u64
            {
                std::thread::sleep(timeout.min(Duration::from_millis(5)));
                return Err(io::Error::new(io::ErrorKind::TimedOut, "fake read timeout"));
            }
            self.read(buffer)
        }
    }

    impl ControlReadHalf for FakeReader {}

    struct FakeWriter {
        output: Arc<Mutex<Vec<u8>>>,
        write_error: Option<String>,
        write_gate: Option<Arc<WriteGate>>,
    }

    impl Write for FakeWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let write_index = self.write_gate.as_ref().map(|gate| gate.begin_write());
            if let Some(error) = &self.write_error {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    error.clone(),
                ));
            }
            self.output
                .lock()
                .expect("fake output lock")
                .extend_from_slice(buffer);
            if let (Some(gate), Some(write_index)) = (&self.write_gate, write_index) {
                gate.finish_write(write_index);
            }
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct WriteGate {
        stalled_write: usize,
        next_write: std::sync::atomic::AtomicUsize,
        started_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release_rx: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        detach_written_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    }

    impl WriteGate {
        fn new(
            stalled_write: usize,
            started_tx: std::sync::mpsc::Sender<()>,
            release_rx: std::sync::mpsc::Receiver<()>,
            detach_written_tx: std::sync::mpsc::Sender<()>,
        ) -> Arc<Self> {
            Arc::new(Self {
                stalled_write,
                next_write: std::sync::atomic::AtomicUsize::new(0),
                started_tx: Mutex::new(Some(started_tx)),
                release_rx: Mutex::new(Some(release_rx)),
                detach_written_tx: Mutex::new(Some(detach_written_tx)),
            })
        }

        fn begin_write(&self) -> usize {
            let write_index = self
                .next_write
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if write_index == self.stalled_write {
                self.started_tx
                    .lock()
                    .expect("write gate started lock")
                    .take()
                    .expect("write gate start signal")
                    .send(())
                    .expect("write gate start receiver");
                self.release_rx
                    .lock()
                    .expect("write gate release lock")
                    .take()
                    .expect("write gate release receiver")
                    .recv()
                    .expect("write gate release signal");
            }
            write_index
        }

        fn finish_write(&self, write_index: usize) {
            // Each frame uses a prefix and payload write; the detach payload
            // is therefore the fourth write after the stalled frame starts.
            if write_index == self.stalled_write + 3 {
                self.detach_written_tx
                    .lock()
                    .expect("detach-written lock")
                    .take()
                    .expect("detach-written signal")
                    .send(())
                    .expect("detach-written receiver");
            }
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "fake broken pipe",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct StalledWriter {
        started_tx: Option<std::sync::mpsc::Sender<()>>,
        release_rx: std::sync::mpsc::Receiver<()>,
        stalled: bool,
    }

    impl Write for StalledWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if !self.stalled {
                self.stalled = true;
                self.started_tx
                    .take()
                    .expect("stalled writer start signal")
                    .send(())
                    .expect("stalled writer start receiver");
                self.release_rx
                    .recv()
                    .expect("stalled writer release signal");
            }
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FakeRunner {
        stream: Mutex<Option<FakeStream>>,
        connect_error: Option<String>,
        connected_targets: Option<Arc<Mutex<Vec<String>>>>,
    }

    struct BlockingRunner {
        inner: FakeRunner,
        connect_started_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        connect_release_rx: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl SshRunner for FakeRunner {
        fn connect(
            &self,
            host: &crate::config::FleetHostConfig,
        ) -> Result<Box<dyn ControlStream>, io::Error> {
            if let Some(error) = &self.connect_error {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    error.clone(),
                ));
            }
            if let Some(targets) = &self.connected_targets {
                targets
                    .lock()
                    .expect("fake targets lock")
                    .push(host.target.clone());
            }
            self.stream
                .lock()
                .expect("fake runner lock")
                .take()
                .map(|stream| Box::new(stream) as Box<dyn ControlStream>)
                .ok_or_else(|| io::Error::other("no fake connection"))
        }
    }

    impl SshRunner for BlockingRunner {
        fn connect(
            &self,
            host: &crate::config::FleetHostConfig,
        ) -> Result<Box<dyn ControlStream>, io::Error> {
            self.connect_started_tx
                .lock()
                .expect("connect-started lock")
                .take()
                .expect("connect-started signal")
                .send(())
                .expect("connect-started receiver");
            self.connect_release_rx
                .lock()
                .expect("connect-release lock")
                .take()
                .expect("connect-release signal")
                .recv()
                .expect("connect-release sender");
            self.inner.connect(host)
        }
    }

    fn framed(message: &impl serde::Serialize) -> Vec<u8> {
        let mut output = Vec::new();
        protocol::write_message(&mut output, message).expect("frame fake message");
        output
    }

    fn test_channels() -> (
        RemoteProxyChannels,
        tokio::sync::mpsc::Sender<ProxyOutbound>,
    ) {
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
        (
            RemoteProxyChannels {
                outbound_rx,
                detach_tx: outbound_tx.clone(),
                resize_slot: Arc::new(Mutex::new((24, 80, 0, 0))),
                input_enabled: Arc::new(AtomicBool::new(false)),
                operation_state: crate::remote::RemoteFocusOperationState::new(),
            },
            outbound_tx,
        )
    }

    fn test_termination(
        detached: Arc<AtomicBool>,
        input_enabled: Arc<AtomicBool>,
    ) -> Arc<ControlSessionTermination> {
        test_termination_with_state(
            detached,
            input_enabled,
            crate::remote::RemoteFocusOperationState::new(),
        )
    }

    fn test_termination_with_state(
        detached: Arc<AtomicBool>,
        input_enabled: Arc<AtomicBool>,
        operation_state: Arc<crate::remote::RemoteFocusOperationState>,
    ) -> Arc<ControlSessionTermination> {
        let sessions = Arc::new(Mutex::new(std::collections::HashMap::new()));
        Arc::new(ControlSessionTermination {
            sessions: Arc::downgrade(&sessions),
            operation_id: "writer-test".into(),
            detached,
            input_enabled,
            operation_state,
        })
    }

    fn transport_with(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
    ) -> SshRemoteFocusTransport {
        transport_with_read_gate(input, output, fail_at, None)
    }

    fn transport_with_panic_at(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        panic_at: u64,
    ) -> SshRemoteFocusTransport {
        transport_with_fake_stream(input, output, None, Some(panic_at), None)
    }

    fn transport_with_read_gate(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
    ) -> SshRemoteFocusTransport {
        transport_with_fake_stream(input, output, fail_at, None, read_gate)
    }

    fn transport_with_fake_stream(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
    ) -> SshRemoteFocusTransport {
        transport_with_writer_options(input, output, fail_at, panic_at, read_gate, None, None)
    }

    fn transport_with_write_error(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        read_gate: Option<Arc<AtomicBool>>,
        write_error: &str,
    ) -> SshRemoteFocusTransport {
        transport_with_writer_options(
            input,
            output,
            None,
            None,
            read_gate,
            Some(write_error.to_owned()),
            None,
        )
    }

    fn transport_with_write_gate(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        read_gate: Option<Arc<AtomicBool>>,
        write_gate: Arc<WriteGate>,
    ) -> SshRemoteFocusTransport {
        transport_with_writer_options(input, output, None, None, read_gate, None, Some(write_gate))
    }

    fn transport_with_writer_options(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
        write_error: Option<String>,
        write_gate: Option<Arc<WriteGate>>,
    ) -> SshRemoteFocusTransport {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "buildbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut transport = SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(Some(FakeStream {
                    input: Cursor::new(input),
                    output,
                    fail_at,
                    panic_at,
                    read_gate,
                    write_error,
                    write_gate,
                    diagnostic: None,
                    handshake_write_gate: None,
                })),
                connect_error: None,
                connected_targets: None,
            }),
        );
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox", "buildbox", "buildbox",
        ));
        transport
    }

    fn transport_with_connect_error(error: &str) -> SshRemoteFocusTransport {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "buildbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut transport = SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(None),
                connect_error: Some(error.to_owned()),
                connected_targets: None,
            }),
        );
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox", "buildbox", "buildbox",
        ));
        transport
    }

    fn remote_identity_snapshot(
        configured_name: &str,
        target: &str,
        remote_name: &str,
    ) -> crate::fleet::Snapshot {
        crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: configured_name.to_owned(),
                target: target.to_owned(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: Some(remote_name.to_owned()),
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: Vec::new(),
            }],
            ..Default::default()
        }
    }

    fn agent_ref() -> AgentRef {
        AgentRef {
            host: "buildbox".into(),
            agent: "claude".into(),
        }
    }

    fn receive_failure(
        event_rx: &mut tokio::sync::mpsc::Receiver<crate::events::AppEvent>,
    ) -> ErrorBody {
        let event = event_rx.blocking_recv().expect("failure event");
        let crate::events::AppEvent::RemoteFocusTransition { transition, .. } = event else {
            panic!("expected remote focus transition");
        };
        let RemoteFocusTransition::Failed(error) = *transition else {
            panic!("expected failed remote focus transition");
        };
        error
    }

    fn control_context() -> crate::api::schema::RemoteControlContext {
        crate::api::schema::RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: "w1:p3".into(),
            terminal_id: "terminal-id".into(),
            cwd: "/work/repo".into(),
            foreground_cwd: "/work/repo".into(),
            tty: "/dev/pts/4".into(),
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into(), "run".into()],
                cwd: "/work/repo".into(),
            },
            detected_agent: "agent".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 9,
            revision: 41,
            context_epoch: 12,
        }
    }

    fn welcome_bytes() -> Vec<u8> {
        framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        })
    }

    fn wire_messages(written: &[u8]) -> Vec<ClientMessage> {
        let mut messages = Vec::new();
        let mut input = written;
        while !input.is_empty() {
            let message: ClientMessage =
                protocol::read_message(&mut input, MAX_FRAME_SIZE).expect("wire message parses");
            messages.push(message);
        }
        messages
    }

    #[test]
    fn version_skew_is_reported_before_control_terminal_is_written() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let input = framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION - 1,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "version_skew");
        let written = output.lock().expect("fake output lock");
        let hello: ClientMessage = protocol::read_message(&mut written.as_slice(), MAX_FRAME_SIZE)
            .expect("hello was written");
        assert!(matches!(hello, ClientMessage::Hello { .. }));
        assert_eq!(written.len(), framed(&hello).len());
    }

    #[test]
    fn build_skew_and_missing_identity_are_rejected_before_control_terminal() {
        for build_version in ["other-build", ""] {
            let output = Arc::new(Mutex::new(Vec::new()));
            let input = framed(&ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                build_version: build_version.to_owned(),
                encoding: RenderEncoding::TerminalAnsi,
                error: None,
            });
            let mut transport = transport_with(input, Arc::clone(&output), None);
            let (channels, _outbound_tx) = test_channels();
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
            transport
                .start("operation", &agent_ref(), "proxy", channels, event_tx)
                .expect("thread starts");
            assert_eq!(receive_failure(&mut event_rx).code, "version_skew");
            let written = output.lock().expect("fake output lock");
            let mut input = written.as_slice();
            let _: ClientMessage =
                protocol::read_message(&mut input, MAX_FRAME_SIZE).expect("hello was written");
            assert!(
                input.is_empty(),
                "control request must not cross version skew"
            );
        }
    }

    #[test]
    fn legacy_v21_welcome_is_classified_as_version_skew() {
        #[derive(serde::Serialize)]
        enum LegacyServerMessageV21 {
            Welcome {
                version: u32,
                encoding: RenderEncoding,
                error: Option<String>,
            },
        }

        let output = Arc::new(Mutex::new(Vec::new()));
        let input = framed(&LegacyServerMessageV21::Welcome {
            version: PROTOCOL_VERSION - 1,
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "version_skew");
    }

    #[test]
    fn ssh_permission_failure_without_provenance_is_host_unreachable() {
        let mut transport = transport_with_connect_error("Permission denied (publickey)");
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "host_unreachable");
    }

    #[test]
    fn ssh_auth_diagnostic_without_provenance_is_host_unreachable() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "buildbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let runner = Arc::new(FakeRunner {
            stream: Mutex::new(Some(FakeStream {
                input: Cursor::new(Vec::new()),
                output: Arc::new(Mutex::new(Vec::new())),
                fail_at: None,
                panic_at: None,
                read_gate: None,
                write_error: None,
                write_gate: None,
                diagnostic: Some("buildbox: Permission denied (publickey).".into()),
                handshake_write_gate: None,
            })),
            connect_error: None,
            connected_targets: None,
        });
        let mut transport = SshRemoteFocusTransport::with_runner(&fleet, runner);
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox", "buildbox", "buildbox",
        ));
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "host_unreachable");
    }

    #[test]
    fn hostile_ssh_target_is_rejected_before_any_process_spawn() {
        let host = crate::config::FleetHostConfig {
            target: "-oProxyCommand=touch /tmp/herdr-owned".into(),
            ..Default::default()
        };
        let result = OpenSshRunner.connect(&host);
        assert!(matches!(
            result,
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn close_diagnostic_kills_a_stalled_remote_process_before_reading_stderr() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "printf diagnostic >&2; exec sleep 30"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("diagnostic child");
        let pid = child.id();
        let stdout = child.stdout.take().expect("diagnostic stdout");
        let stderr = child.stderr.take().expect("diagnostic stderr");
        let mut reader = ProcessControlReader {
            child,
            stdout,
            stderr: Some(stderr),
        };
        std::thread::sleep(Duration::from_millis(50));
        let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            diagnostic_tx
                .send(reader.close_diagnostic())
                .expect("diagnostic result");
        });

        match diagnostic_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(diagnostic) => {
                assert!(diagnostic.is_some_and(|text| text.contains("diagnostic")));
            }
            Err(error) => {
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
                panic!("close_diagnostic stalled: {error}");
            }
        }
        reader_thread.join().expect("diagnostic thread");
    }

    #[test]
    // AC7: the control SSH command is non-interactive and separates the target from the bridge command.
    fn control_ssh_argv_has_batch_mode_and_command_separator() {
        let host = crate::config::FleetHostConfig {
            target: "buildbox".into(),
            ..Default::default()
        };
        let command = ssh_command(&host);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("SSH target separator");
        assert_eq!(
            args.get(separator + 1).map(String::as_str),
            Some("buildbox")
        );
        assert_eq!(
            &args[separator + 2..],
            ["herdr remote-control-bridge".to_owned()]
        );
    }

    #[test]
    fn control_ssh_argv_honors_socket_and_session() {
        let host = crate::config::FleetHostConfig {
            target: "operator@buildbox".into(),
            socket: Some("/run/herdr remote.sock".into()),
            session: Some("agents-main".into()),
            ..Default::default()
        };
        let command = ssh_command(&host);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("SSH target separator");
        assert_eq!(args[separator + 1], "operator@buildbox");
        assert_eq!(
            args[separator + 2],
            "HERDR_SOCKET_PATH='/run/herdr remote.sock' HERDR_SESSION='agents-main' herdr remote-control-bridge"
        );
    }

    #[test]
    fn control_ssh_argv_makes_named_session_selection_explicit() {
        let host = crate::config::FleetHostConfig {
            target: "operator@buildbox".into(),
            session: Some("agents-main".into()),
            ..Default::default()
        };
        let command = ssh_command(&host);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("SSH target separator");

        assert_eq!(
            args[separator + 2],
            "herdr --session 'agents-main' remote-control-bridge"
        );
    }

    #[test]
    // AC7: remote focus only resolves the admitted hosts returned by select_hosts.
    fn remote_focus_transport_resolves_hosts_through_select_hosts() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![
                crate::config::FleetHostConfig {
                    name: "laptop".into(),
                    local: true,
                    ..Default::default()
                },
                crate::config::FleetHostConfig {
                    name: "buildbox".into(),
                    target: "operator@buildbox".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let transport = SshRemoteFocusTransport::new(&fleet);
        assert_eq!(
            transport
                .hosts
                .get("buildbox")
                .map(|host| host.target.as_str()),
            Some("operator@buildbox")
        );
        assert!(!transport.hosts.contains_key("laptop"));

        let invalid = crate::config::FleetConfig {
            hosts: vec![
                crate::config::FleetHostConfig {
                    name: "duplicate".into(),
                    target: "one".into(),
                    ..Default::default()
                },
                crate::config::FleetHostConfig {
                    name: "duplicate".into(),
                    target: "two".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(SshRemoteFocusTransport::new(&invalid).hosts.is_empty());
    }

    #[test]
    fn configured_alias_is_translated_to_remote_identity_on_the_wire() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut remote_context = control_context();
        remote_context.host = "ubuntu-direct".into();
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(remote_context.clone()),
        }));
        let mut transport = SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(Some(FakeStream {
                    input: Cursor::new(input),
                    output: Arc::clone(&output),
                    fail_at: None,
                    panic_at: None,
                    read_gate: None,
                    write_error: None,
                    write_gate: None,
                    diagnostic: None,
                    handshake_write_gate: None,
                })),
                connect_error: None,
                connected_targets: None,
            }),
        );
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "ub1",
            "operator@ub1",
            "ubuntu-direct",
        ));

        let mut expected_context = control_context();
        expected_context.host = "ub1".into();
        let requested = AgentRef::new("ub1", "w1:pA").expect("configured agent reference");
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start_with_expected_context_and_version_for_test(
                "operation",
                &requested,
                Some(expected_context),
                PROTOCOL_VERSION,
                channels,
                event_tx,
            )
            .expect("thread starts");

        let Some(crate::events::AppEvent::RemoteFocusTransition { transition, .. }) =
            event_rx.blocking_recv()
        else {
            panic!("expected active transition");
        };
        let RemoteFocusTransition::Active(context) = *transition else {
            panic!("expected active transition");
        };
        assert_eq!(context.host, "ub1");

        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert!(matches!(
            messages.first(),
            Some(ClientMessage::Hello { .. })
        ));
        assert!(matches!(
            messages.get(1),
            Some(ClientMessage::ControlTerminal {
                target,
                agent_ref: Some(wire_ref),
                expected_context: Some(expected),
                takeover: false,
            }) if target == "ubuntu-direct::w1:pA"
                && wire_ref == &AgentRef::new("ubuntu-direct", "w1:pA").expect("wire ref")
                && expected.host == "ubuntu-direct"
        ));
    }

    #[test]
    fn unexpected_remote_context_host_is_refused_before_reaching_the_app() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut context = control_context();
        context.host = "unexpected-host".into();
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(context),
        }));
        let mut transport = SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(Some(FakeStream {
                    input: Cursor::new(input),
                    output,
                    fail_at: None,
                    panic_at: None,
                    read_gate: None,
                    write_error: None,
                    write_gate: None,
                    diagnostic: None,
                    handshake_write_gate: None,
                })),
                connect_error: None,
                connected_targets: None,
            }),
        );
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "ub1",
            "operator@ub1",
            "ubuntu-direct",
        ));

        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start(
                "operation",
                &AgentRef::new("ub1", "w1:pA").expect("agent ref"),
                "proxy",
                channels,
                event_tx,
            )
            .expect("thread starts");

        let error = receive_failure(&mut event_rx);
        assert_eq!(error.code, "refused_for_safety");
    }

    #[test]
    // A6: the remote may return only the learned identity, not the configured alias.
    fn configured_alias_from_remote_is_refused_when_identity_differs() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut context = control_context();
        context.host = "ub1".into();
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(context),
        }));
        let mut transport = SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(Some(FakeStream {
                    input: Cursor::new(input),
                    output,
                    fail_at: None,
                    panic_at: None,
                    read_gate: None,
                    write_error: None,
                    write_gate: None,
                    diagnostic: None,
                    handshake_write_gate: None,
                })),
                connect_error: None,
                connected_targets: None,
            }),
        );
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "ub1",
            "operator@ub1",
            "ubuntu-direct",
        ));

        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start(
                "operation",
                &AgentRef::new("ub1", "w1:pA").expect("agent ref"),
                "proxy",
                channels,
                event_tx,
            )
            .expect("thread starts");

        let error = receive_failure(&mut event_rx);
        assert_eq!(error.code, "refused_for_safety");
    }

    #[test]
    // A3: a learned identity is valid only for its configured socket and session.
    fn remote_identity_is_bound_to_socket_and_session() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                socket: Some("/run/herdr.sock".into()),
                session: Some("agents".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut transport = SshRemoteFocusTransport::new(&fleet);
        let mut valid = remote_identity_snapshot("ub1", "operator@ub1", "ubuntu-direct");
        valid.hosts[0].socket = Some("/run/herdr.sock".into());
        valid.hosts[0].session = Some("agents".into());
        transport.observe_fleet_snapshot(&valid);
        assert!(transport.remote_host_ready("ub1"));

        for (socket, session) in [
            (Some("/run/other.sock"), Some("agents")),
            (Some("/run/herdr.sock"), Some("other-session")),
        ] {
            let mut mismatched = valid.clone();
            mismatched.hosts[0].socket = socket.map(str::to_owned);
            mismatched.hosts[0].session = session.map(str::to_owned);
            transport.observe_fleet_snapshot(&mismatched);
            assert!(!transport.remote_host_ready("ub1"));
            transport.observe_fleet_snapshot(&valid);
            assert!(transport.remote_host_ready("ub1"));
        }
    }

    #[test]
    fn failed_or_ambiguous_fleet_observations_clear_remote_focus_readiness() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut transport = SshRemoteFocusTransport::new(&fleet);
        let mut valid = remote_identity_snapshot("ub1", "operator@ub1", "ubuntu-direct");
        transport.observe_fleet_snapshot(&valid);
        assert!(transport.remote_host_ready("ub1"));

        for state in [
            crate::fleet::HostState::VersionSkew,
            crate::fleet::HostState::Unreachable,
        ] {
            valid.hosts[0].state = state;
            transport.observe_fleet_snapshot(&valid);
            assert!(
                !transport.remote_host_ready("ub1"),
                "{state:?} observations must clear readiness"
            );
            valid.hosts[0].state = crate::fleet::HostState::Reachable;
            transport.observe_fleet_snapshot(&valid);
            assert!(transport.remote_host_ready("ub1"));
        }

        valid.hosts[0].target = "different-target".into();
        transport.observe_fleet_snapshot(&valid);
        assert!(!transport.remote_host_ready("ub1"));

        valid.hosts[0].target = "operator@ub1".into();
        valid.hosts[0].remote_identity = None;
        transport.observe_fleet_snapshot(&valid);
        assert!(!transport.remote_host_ready("ub1"));

        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "ub1",
            "operator@ub1",
            "ubuntu-direct",
        ));
        assert!(transport.remote_host_ready("ub1"));
        transport.observe_fleet_snapshot(&crate::fleet::Snapshot::default());
        assert!(!transport.remote_host_ready("ub1"));
    }

    #[test]
    fn duplicate_host_rows_clear_remote_focus_readiness() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut transport = SshRemoteFocusTransport::new(&fleet);
        let first = remote_identity_snapshot("ub1", "operator@ub1", "ubuntu-direct");
        transport.observe_fleet_snapshot(&first);
        assert!(transport.remote_host_ready("ub1"));

        let mut duplicate = first.clone();
        duplicate.hosts.push(first.hosts[0].clone());
        transport.observe_fleet_snapshot(&duplicate);
        assert!(!transport.remote_host_ready("ub1"));

        transport.observe_fleet_snapshot(&first);
        assert!(transport.remote_host_ready("ub1"));
    }

    #[test]
    fn unchanged_fleet_reload_keeps_remote_focus_identity_without_poll() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "operator@buildbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let connected_targets = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut context = control_context();
        context.host = "remote-buildbox".into();
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(context),
        }));
        let runner = Arc::new(FakeRunner {
            stream: Mutex::new(Some(FakeStream {
                input: Cursor::new(input),
                output: Arc::clone(&output),
                fail_at: None,
                panic_at: None,
                read_gate: None,
                write_error: None,
                write_gate: None,
                diagnostic: None,
                handshake_write_gate: None,
            })),
            connect_error: None,
            connected_targets: Some(Arc::clone(&connected_targets)),
        });
        let mut transport = SshRemoteFocusTransport::with_runner(&fleet, runner);
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox",
            "operator@buildbox",
            "remote-buildbox",
        ));

        assert!(transport.reload_fleet(&fleet).is_empty());
        assert!(transport.remote_host_ready("buildbox"));

        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("unchanged host remains focusable without another poll");
        assert!(matches!(
            event_rx.blocking_recv(),
            Some(crate::events::AppEvent::RemoteFocusTransition { transition, .. })
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        assert_eq!(receive_failure(&mut event_rx).code, "connection_lost");
        assert_eq!(
            connected_targets.lock().expect("targets lock").as_slice(),
            ["operator@buildbox"]
        );
        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert!(matches!(
            messages.get(1),
            Some(ClientMessage::ControlTerminal {
                agent_ref: Some(wire_ref), ..
            }) if wire_ref.host == "remote-buildbox"
        ));
    }

    #[test]
    fn changed_or_removed_host_clears_remote_identity_on_reload() {
        let original_host = crate::config::FleetHostConfig {
            name: "ub1".into(),
            target: "operator@ub1".into(),
            socket: Some("/run/herdr.sock".into()),
            session: Some("agents".into()),
            ..Default::default()
        };
        let mut changed_hosts = Vec::new();
        let mut renamed = original_host.clone();
        renamed.name = "renamed".into();
        changed_hosts.push(vec![renamed]);
        let mut retargeted = original_host.clone();
        retargeted.target = "operator@other".into();
        changed_hosts.push(vec![retargeted]);
        let mut resocketed = original_host.clone();
        resocketed.socket = Some("/run/other.sock".into());
        changed_hosts.push(vec![resocketed]);
        let mut resessioned = original_host.clone();
        resessioned.session = Some("other".into());
        changed_hosts.push(vec![resessioned]);
        changed_hosts.push(Vec::new());

        for hosts in changed_hosts {
            let original_fleet = crate::config::FleetConfig {
                hosts: vec![original_host.clone()],
                ..Default::default()
            };
            let mut transport = SshRemoteFocusTransport::new(&original_fleet);
            let mut observed = remote_identity_snapshot("ub1", "operator@ub1", "remote-ub1");
            observed.hosts[0].socket = Some("/run/herdr.sock".into());
            observed.hosts[0].session = Some("agents".into());
            transport.observe_fleet_snapshot(&observed);
            assert!(transport.remote_host_ready("ub1"));

            let changed_fleet = crate::config::FleetConfig {
                hosts,
                ..Default::default()
            };
            assert!(transport.reload_fleet(&changed_fleet).is_empty());
            assert!(!transport.remote_host_ready("ub1"));
            assert!(!transport.remote_host_ready("renamed"));
        }
    }

    #[test]
    fn stale_same_tuple_observation_cannot_restore_identity_after_reload() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut transport = SshRemoteFocusTransport::new(&fleet);
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "ub1",
            "operator@ub1",
            "old-remote",
        ));
        assert!(transport.remote_host_ready("ub1"));

        transport.reload_fleet(&fleet);
        assert!(transport.remote_host_ready("ub1"));

        let current_failed = crate::fleet::Snapshot {
            config_generation: 1,
            ..Default::default()
        };
        transport.observe_fleet_snapshot(&current_failed);
        assert!(!transport.remote_host_ready("ub1"));

        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "ub1",
            "operator@ub1",
            "old-remote",
        ));
        assert!(!transport.remote_host_ready("ub1"));

        let mut current_snapshot = remote_identity_snapshot("ub1", "operator@ub1", "new-remote");
        current_snapshot.config_generation = 1;
        transport.observe_fleet_snapshot(&current_snapshot);
        assert!(transport.remote_host_ready("ub1"));
    }

    #[test]
    fn unknown_remote_identity_fails_before_connecting_or_writing_control() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub1".into(),
                target: "operator@ub1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let connected_targets = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut transport = SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(Some(FakeStream {
                    input: Cursor::new(welcome_bytes()),
                    output: Arc::clone(&output),
                    fail_at: None,
                    panic_at: None,
                    read_gate: None,
                    write_error: None,
                    write_gate: None,
                    diagnostic: None,
                    handshake_write_gate: None,
                })),
                connect_error: None,
                connected_targets: Some(Arc::clone(&connected_targets)),
            }),
        );
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(2);
        let error = transport
            .start(
                "operation",
                &AgentRef::new("ub1", "w1:pA").expect("agent ref"),
                "proxy",
                channels,
                event_tx,
            )
            .expect_err("unknown identity must fail closed");

        assert_eq!(error.code, "host_unreachable");
        assert!(error.message.contains("no unambiguous identity"));
        assert!(connected_targets.lock().expect("targets lock").is_empty());
        assert!(output.lock().expect("fake output lock").is_empty());
    }

    #[test]
    fn reloading_fleet_cannot_send_an_old_identity_to_a_new_target() {
        let old_fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "old-target".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let new_fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "new-target".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let connected_targets = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(FakeRunner {
            stream: Mutex::new(Some(FakeStream {
                input: Cursor::new(welcome_bytes()),
                output: Arc::clone(&output),
                fail_at: None,
                panic_at: None,
                read_gate: None,
                write_error: None,
                write_gate: None,
                diagnostic: None,
                handshake_write_gate: None,
            })),
            connect_error: None,
            connected_targets: Some(Arc::clone(&connected_targets)),
        });
        let mut transport = SshRemoteFocusTransport::with_runner(&old_fleet, runner);
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox",
            "old-target",
            "old-remote",
        ));
        assert!(transport.accepts_remote_host("buildbox"));
        assert!(transport.reload_fleet(&new_fleet).is_empty());
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox",
            "old-target",
            "old-remote",
        ));

        let (channels, _outbound_tx) = test_channels();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(2);
        let error = transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect_err("stale identity must fail closed");
        assert_eq!(error.code, "host_unreachable");
        assert!(error.message.contains("no unambiguous identity"));
        assert!(connected_targets.lock().expect("targets lock").is_empty());
        assert!(output.lock().expect("fake output lock").is_empty());

        let mut current_snapshot = remote_identity_snapshot("buildbox", "new-target", "new-remote");
        current_snapshot.config_generation = 1;
        transport.observe_fleet_snapshot(&current_snapshot);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation-2", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts after current identity is observed");
        assert_eq!(receive_failure(&mut event_rx).code, "host_unreachable");
        assert_eq!(
            connected_targets.lock().expect("targets lock").as_slice(),
            ["new-target"]
        );
        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert!(matches!(
            messages.get(1),
            Some(ClientMessage::ControlTerminal {
                agent_ref: Some(wire_ref), ..
            }) if wire_ref.host == "new-remote"
        ));
    }

    #[test]
    fn reloading_changed_host_during_connect_does_not_request_old_control() {
        let old_fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "old-target".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let new_fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "new-target".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let (connect_started_tx, connect_started_rx) = std::sync::mpsc::channel();
        let (connect_release_tx, connect_release_rx) = std::sync::mpsc::channel();
        let runner = Arc::new(BlockingRunner {
            inner: FakeRunner {
                stream: Mutex::new(Some(FakeStream {
                    input: Cursor::new(welcome_bytes()),
                    output: Arc::clone(&output),
                    fail_at: None,
                    panic_at: None,
                    read_gate: None,
                    write_error: None,
                    write_gate: None,
                    diagnostic: None,
                    handshake_write_gate: None,
                })),
                connect_error: None,
                connected_targets: None,
            },
            connect_started_tx: Mutex::new(Some(connect_started_tx)),
            connect_release_rx: Mutex::new(Some(connect_release_rx)),
        });
        let mut transport = SshRemoteFocusTransport::with_runner(&old_fleet, runner);
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox",
            "old-target",
            "old-remote",
        ));
        let (channels, _outbound_tx) = test_channels();
        let operation_state = Arc::clone(&channels.operation_state);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);

        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        connect_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("connect starts");
        assert_eq!(transport.reload_fleet(&new_fleet), vec!["operation"]);
        assert!(operation_state.is_terminal());
        connect_release_tx.send(()).expect("release connect");

        assert!(
            event_rx.blocking_recv().is_none(),
            "revoked connect must not report an active or duplicate failure transition"
        );
        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert!(matches!(messages.as_slice(), [ClientMessage::Hello { .. }]));
    }

    #[test]
    fn reloading_changed_host_while_control_request_is_on_the_wire_detaches() {
        let old_fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "old-target".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let new_fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "new-target".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let (write_started_tx, write_started_rx) = std::sync::mpsc::channel();
        let (write_release_tx, write_release_rx) = std::sync::mpsc::channel();
        let runner = Arc::new(FakeRunner {
            stream: Mutex::new(Some(FakeStream {
                input: Cursor::new(welcome_bytes()),
                output: Arc::clone(&output),
                fail_at: None,
                panic_at: None,
                read_gate: None,
                write_error: None,
                write_gate: None,
                diagnostic: None,
                // Hello is writes 0-1; the ControlTerminal payload is write 3.
                handshake_write_gate: Some(HandshakeWriteGate {
                    stalled_write: 3,
                    next_write: 0,
                    started_tx: write_started_tx,
                    release_rx: write_release_rx,
                }),
            })),
            connect_error: None,
            connected_targets: None,
        });
        let mut transport = SshRemoteFocusTransport::with_runner(&old_fleet, runner);
        transport.observe_fleet_snapshot(&remote_identity_snapshot(
            "buildbox",
            "old-target",
            "old-remote",
        ));
        let (channels, _outbound_tx) = test_channels();
        let operation_state = Arc::clone(&channels.operation_state);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);

        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        write_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("control request write starts");
        assert_eq!(transport.reload_fleet(&new_fleet), vec!["operation"]);
        assert!(operation_state.is_terminal());
        write_release_tx.send(()).expect("release write");

        assert!(
            event_rx.blocking_recv().is_none(),
            "revoked request must not report an active or duplicate failure transition"
        );
        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert!(
            matches!(
                messages.as_slice(),
                [
                    ClientMessage::Hello { .. },
                    ClientMessage::ControlTerminal { .. },
                    ClientMessage::Detach
                ]
            ),
            "a lease granted to the revoked request must be released: {messages:?}"
        );
    }

    #[test]
    fn reloading_changed_host_revokes_the_existing_session_and_gate() {
        let new_fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "new-target".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport =
            transport_with_read_gate(input, output, None, Some(Arc::clone(&read_gate)));
        let (channels, _outbound_tx) = test_channels();
        let input_enabled = Arc::clone(&channels.input_enabled);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        assert!(matches!(
            event_rx.blocking_recv(),
            Some(crate::events::AppEvent::RemoteFocusTransition { transition, .. })
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));

        assert_eq!(transport.reload_fleet(&new_fleet), vec!["operation"]);
        assert!(!input_enabled.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !transport.sessions.lock().expect("sessions lock").is_empty() {
            assert!(
                Instant::now() < deadline,
                "revoked session remained registered"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        read_gate.store(true, Ordering::Release);
    }

    #[test]
    fn stalled_initial_welcome_read_is_bounded() {
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut stream = FakeStream {
            input: Cursor::new(Vec::new()),
            output: Arc::new(Mutex::new(Vec::new())),
            fail_at: None,
            panic_at: None,
            read_gate: Some(read_gate),
            write_error: None,
            write_gate: None,
            diagnostic: None,
            handshake_write_gate: None,
        };
        let started = Instant::now();
        let result = read_initial_welcome(&mut stream, started + Duration::from_millis(20));
        assert!(matches!(
            result,
            Err(protocol::FramingError::Io(error))
                if error.kind() == io::ErrorKind::TimedOut
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn stalled_control_ready_read_is_bounded() {
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut reader = FakeReader {
            input: Cursor::new(Vec::new()),
            fail_at: None,
            panic_at: None,
            read_gate: Some(read_gate),
        };
        let started = Instant::now();
        let result = read_message_with_deadline(&mut reader, started + Duration::from_millis(20));
        assert!(matches!(
            result,
            Err(protocol::FramingError::Io(error))
                if error.kind() == io::ErrorKind::TimedOut
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    // AC6: a typed server ControlError becomes the same typed client failure as other transport errors.
    fn typed_control_error_is_classified_by_the_client() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let welcome = framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let control_error = framed(&ServerMessage::ControlError {
            code: "refused_for_safety".into(),
            message: "controlled PTY write gate refused the batch".into(),
        });
        let mut input = welcome;
        input.extend(control_error);
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let error = receive_failure(&mut event_rx);
        assert_eq!(error.code, "refused_for_safety");
        assert_eq!(error.message, "controlled PTY write gate refused the batch");
    }

    #[test]
    fn mid_stream_loss_reports_unknown_delivery_without_replay() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let welcome = framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let ready = framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        });
        let fail_at = (welcome.len() + ready.len()) as u64;
        let mut input = welcome;
        input.extend(ready);
        let mut transport = transport_with(input, Arc::clone(&output), Some(fail_at));
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        let second = event_rx.blocking_recv().expect("loss event");
        assert!(matches!(
            second,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Failed(ref error)
                    if error.code == "connection_lost" && error.message.contains("delivery")
                )
        ));
        let written = output.lock().expect("fake output lock");
        let mut frames = written.as_slice();
        let _: ClientMessage =
            protocol::read_message(&mut frames, MAX_FRAME_SIZE).expect("hello was written");
        let _: ClientMessage = protocol::read_message(&mut frames, MAX_FRAME_SIZE)
            .expect("control request was written");
        assert!(frames.is_empty(), "loss must not cause a replay");
    }

    #[test]
    fn reader_loss_closes_input_gate_before_failure_event_is_delivered() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport =
            transport_with_read_gate(input, output, None, Some(Arc::clone(&read_gate)));
        let (channels, _outbound_tx) = test_channels();
        let input_enabled = Arc::clone(&channels.input_enabled);
        let operation_state = Arc::clone(&channels.operation_state);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        transport
            .start(
                "operation",
                &agent_ref(),
                "proxy",
                channels,
                event_tx.clone(),
            )
            .expect("thread starts");
        assert!(matches!(
            event_rx.blocking_recv(),
            Some(crate::events::AppEvent::RemoteFocusTransition { transition, .. })
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));

        event_tx
            .blocking_send(crate::events::AppEvent::RemoteFocusFrame {
                operation_id: "sentinel".into(),
                frame: Box::new(crate::protocol::TerminalFrame {
                    seq: 0,
                    width: 1,
                    height: 1,
                    full: true,
                    bytes: Vec::new(),
                }),
            })
            .expect("hold failure event behind a pending app event");
        input_enabled.store(true, Ordering::Release);
        read_gate.store(true, Ordering::Release);

        let deadline = Instant::now() + Duration::from_secs(2);
        while input_enabled.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "reader loss did not close the input gate before reporting failure"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!input_enabled.load(Ordering::Acquire));
        assert!(operation_state.is_terminal());
        assert!(
            transport.sessions.lock().expect("sessions lock").is_empty(),
            "reader loss must unregister the session before failure delivery"
        );

        let _sentinel = event_rx.blocking_recv().expect("sentinel event");
        assert_eq!(receive_failure(&mut event_rx).code, "connection_lost");
    }

    #[test]
    fn writer_failure_releases_the_session_and_reports_unknown_delivery() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport = transport_with_write_error(
            input,
            Arc::clone(&output),
            Some(Arc::clone(&read_gate)),
            "injected writer failure",
        );
        let (channels, outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        outbound_tx
            .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"answer")))
            .expect("input queued");

        let deadline = Instant::now() + Duration::from_secs(2);
        let error = loop {
            match event_rx.try_recv() {
                Ok(crate::events::AppEvent::RemoteFocusTransition { transition, .. }) => {
                    if let RemoteFocusTransition::Failed(error) = *transition {
                        break error;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "writer failure did not reach the app"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    panic!("event channel closed before writer failure");
                }
            }
        };
        assert_eq!(error.code, "connection_lost");
        assert!(error.message.contains("injected writer failure"));
        assert!(transport.sessions.lock().expect("sessions lock").is_empty());
        read_gate.store(true, Ordering::Release);
    }

    #[test]
    fn detach_returns_during_stalled_input_or_resize_and_writes_detach() {
        for stall_resize in [false, true] {
            let output = Arc::new(Mutex::new(Vec::new()));
            let mut input = welcome_bytes();
            input.extend(framed(&ServerMessage::ControlReady {
                context: Box::new(control_context()),
            }));
            let read_gate = Arc::new(AtomicBool::new(false));
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let (detach_written_tx, detach_written_rx) = std::sync::mpsc::channel();
            let write_gate = WriteGate::new(0, started_tx, release_rx, detach_written_tx);
            let transport = transport_with_write_gate(
                input,
                Arc::clone(&output),
                Some(Arc::clone(&read_gate)),
                write_gate,
            );
            let transport = Arc::new(Mutex::new(transport));
            let (channels, outbound_tx) = test_channels();
            let resize_slot = Arc::clone(&channels.resize_slot);
            let input_enabled = Arc::clone(&channels.input_enabled);
            let operation_state = Arc::clone(&channels.operation_state);
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
            transport
                .lock()
                .expect("transport lock")
                .start("operation", &agent_ref(), "proxy", channels, event_tx)
                .expect("thread starts");
            let first = event_rx.blocking_recv().expect("active event");
            assert!(matches!(
                first,
                crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                    if matches!(*transition, RemoteFocusTransition::Active(_))
            ));

            if stall_resize {
                *resize_slot.lock().expect("resize slot lock") = (30, 100, 9, 18);
                outbound_tx
                    .blocking_send(ProxyOutbound::SyncResize)
                    .expect("resize queued");
            } else {
                outbound_tx
                    .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"answer")))
                    .expect("input queued");
            }
            started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("writer reached the stalled input or resize write");

            let (detach_done_tx, detach_done_rx) = std::sync::mpsc::channel();
            let detach_transport = Arc::clone(&transport);
            let detach_thread = std::thread::spawn(move || {
                detach_transport
                    .lock()
                    .expect("transport lock")
                    .detach("operation");
                detach_done_tx.send(()).expect("detach completion receiver");
            });
            let detach_returned = detach_done_rx.recv_timeout(Duration::from_secs(2)).is_ok();
            if detach_returned {
                assert!(
                    operation_state.is_terminal(),
                    "detach must terminate the operation"
                );
                assert!(
                    !input_enabled.load(Ordering::Acquire),
                    "detach must close the input gate"
                );
                assert!(
                    transport
                        .lock()
                        .expect("transport lock")
                        .sessions
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .is_empty(),
                    "detach must unregister the session before releasing the write"
                );
                outbound_tx
                    .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"late")))
                    .expect("late input queued");
            }
            release_tx.send(()).expect("release stalled write");
            detach_thread.join().expect("detach thread");
            assert!(
                detach_returned,
                "detach waited for the stalled {} write",
                if stall_resize { "resize" } else { "input" }
            );

            detach_written_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("detach reached the wire");
            let sessions_empty = transport
                .lock()
                .expect("transport lock")
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty();
            assert!(sessions_empty, "detach must release the remote lease");

            let messages = wire_messages(&output.lock().expect("fake output lock"));
            assert!(matches!(messages[0], ClientMessage::Hello { .. }));
            assert!(matches!(messages[1], ClientMessage::ControlTerminal { .. }));
            if stall_resize {
                assert!(matches!(
                    messages[2],
                    ClientMessage::Resize {
                        cols: 100,
                        rows: 30,
                        cell_width_px: 9,
                        cell_height_px: 18,
                    }
                ));
            } else {
                assert!(matches!(
                    &messages[2],
                    ClientMessage::Input { data } if data == b"answer"
                ));
            }
            assert!(matches!(messages[3], ClientMessage::Detach));
            assert!(
                !messages.iter().any(
                    |message| matches!(message, ClientMessage::Input { data } if data == b"late")
                ),
                "input queued after detach must not reach the wire"
            );

            read_gate.store(true, Ordering::Release);
            drop(outbound_tx);
            drop(event_rx);
        }
    }

    #[test]
    fn cancellable_reader_stops_between_partial_frame_reads_after_detach() {
        let detached = AtomicBool::new(false);
        let mut reader = FakeReader {
            input: Cursor::new(b"partial".to_vec()),
            fail_at: None,
            panic_at: None,
            read_gate: None,
        };
        let mut reader = CancellableReader {
            reader: &mut reader,
            detached: &detached,
        };
        let mut first = [0_u8; 1];
        assert_eq!(reader.read(&mut first).expect("first byte"), 1);

        detached.store(true, Ordering::Release);
        let mut second = [0_u8; 1];
        assert!(matches!(
            reader.read(&mut second),
            Err(error) if error.kind() == io::ErrorKind::ConnectionAborted
        ));
    }

    #[test]
    // The proxy pane's screen is the remote terminal: every complete Terminal
    // frame becomes one ordered frame event for the app.
    fn terminal_frames_are_forwarded_as_ordered_frame_events() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let frame = crate::protocol::TerminalFrame {
            seq: 1,
            width: 80,
            height: 24,
            full: true,
            bytes: b"\x1b[1;1Hremote screen".to_vec(),
        };
        input.extend(framed(&ServerMessage::Terminal(frame.clone())));
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        let second = event_rx.blocking_recv().expect("frame event");
        let crate::events::AppEvent::RemoteFocusFrame {
            operation_id,
            frame: received,
        } = second
        else {
            panic!("expected remote focus frame, got {second:?}");
        };
        assert_eq!(operation_id, "operation");
        assert_eq!(*received, frame);
    }

    #[test]
    fn poisoned_resize_slot_sends_the_recovered_geometry() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(2);
        let resize_slot = Arc::new(Mutex::new((30, 100, 9, 18)));
        let poison_slot = Arc::clone(&resize_slot);
        assert!(std::thread::spawn(move || {
            let _guard = poison_slot.lock().expect("resize slot starts healthy");
            panic!("poison resize slot");
        })
        .join()
        .is_err());
        outbound_tx
            .blocking_send(ProxyOutbound::SyncResize)
            .expect("resize queued");
        outbound_tx
            .blocking_send(ProxyOutbound::Detach)
            .expect("detach queued");

        run_control_writer(
            Box::new(FakeWriter {
                output: Arc::clone(&output),
                write_error: None,
                write_gate: None,
            }),
            outbound_rx,
            resize_slot,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(())),
            Arc::new(AtomicBool::new(false)),
            tokio::sync::mpsc::channel(1).0,
            "writer-test".into(),
            test_termination(
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ),
        );

        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert!(matches!(
            messages.as_slice(),
            [
                ClientMessage::Resize {
                    cols: 100,
                    rows: 30,
                    cell_width_px: 9,
                    cell_height_px: 18,
                },
                ClientMessage::Detach,
            ]
        ));
    }

    #[test]
    fn writer_failure_reports_unknown_delivery_and_closes_input_gate() {
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
        outbound_tx
            .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"answer")))
            .expect("input queued");
        let input_enabled = Arc::new(AtomicBool::new(true));
        let detached = Arc::new(AtomicBool::new(false));
        let termination = test_termination(Arc::clone(&detached), Arc::clone(&input_enabled));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);

        run_control_writer(
            Box::new(FailingWriter),
            outbound_rx,
            Arc::new(Mutex::new((24, 80, 0, 0))),
            None,
            Arc::clone(&detached),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(())),
            Arc::new(AtomicBool::new(false)),
            event_tx,
            "operation".into(),
            termination,
        );

        assert!(detached.load(Ordering::Acquire));
        assert!(!input_enabled.load(Ordering::Acquire));
        let Some(crate::events::AppEvent::RemoteFocusTransition { transition, .. }) =
            event_rx.blocking_recv()
        else {
            panic!("writer failure event");
        };
        let RemoteFocusTransition::Failed(error) = *transition else {
            panic!("expected writer failure");
        };
        assert_eq!(error.code, "connection_lost");
        assert!(error.message.contains("delivery"));
    }

    #[test]
    fn writer_drops_input_when_loss_wins_before_wire_admission() {
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
        outbound_tx
            .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(
                b"late input",
            )))
            .expect("input queued");
        drop(outbound_tx);
        let output = Arc::new(Mutex::new(Vec::new()));
        let input_enabled = Arc::new(AtomicBool::new(true));
        let detached = Arc::new(AtomicBool::new(false));
        let operation_state = crate::remote::RemoteFocusOperationState::new();
        let state_for_hook = Arc::clone(&operation_state);
        operation_state.set_input_send_hook(Arc::new(move || {
            assert!(
                state_for_hook.terminate(),
                "the hook must simulate first loss"
            );
        }));
        let termination = test_termination_with_state(
            Arc::clone(&detached),
            Arc::clone(&input_enabled),
            operation_state,
        );

        run_control_writer(
            Box::new(FakeWriter {
                output: Arc::clone(&output),
                write_error: None,
                write_gate: None,
            }),
            outbound_rx,
            Arc::new(Mutex::new((24, 80, 0, 0))),
            None,
            detached,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(())),
            Arc::new(AtomicBool::new(false)),
            tokio::sync::mpsc::channel(1).0,
            "operation".into(),
            termination,
        );

        assert!(wire_messages(&output.lock().expect("fake output lock")).is_empty());
    }

    #[test]
    fn writer_does_not_hold_admission_while_stdin_write_is_stalled() {
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
        outbound_tx
            .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"blocked")))
            .expect("input queued");
        drop(outbound_tx);

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let input_enabled = Arc::new(AtomicBool::new(true));
        let detached = Arc::new(AtomicBool::new(false));
        let operation_state = crate::remote::RemoteFocusOperationState::new();
        let termination = test_termination_with_state(
            Arc::clone(&detached),
            Arc::clone(&input_enabled),
            operation_state,
        );
        let termination_for_writer = Arc::clone(&termination);
        let writer_thread = std::thread::spawn(move || {
            run_control_writer(
                Box::new(StalledWriter {
                    started_tx: Some(started_tx),
                    release_rx,
                    stalled: false,
                }),
                outbound_rx,
                Arc::new(Mutex::new((24, 80, 0, 0))),
                None,
                detached,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(())),
                Arc::new(AtomicBool::new(false)),
                tokio::sync::mpsc::channel(1).0,
                "operation".into(),
                termination_for_writer,
            );
        });

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer reached the stalled stdin write");

        let (terminated_tx, terminated_rx) = std::sync::mpsc::channel();
        let termination_for_teardown = Arc::clone(&termination);
        let teardown_thread = std::thread::spawn(move || {
            termination_for_teardown.terminate();
            terminated_tx
                .send(())
                .expect("termination completion receiver");
        });
        let terminated_before_write_release = terminated_rx
            .recv_timeout(Duration::from_millis(250))
            .is_ok();

        release_tx.send(()).expect("release stalled stdin write");
        writer_thread.join().expect("writer thread completes");
        teardown_thread.join().expect("teardown thread completes");

        assert!(
            terminated_before_write_release,
            "termination waited for the stalled stdin write"
        );
    }

    #[test]
    fn dropped_resize_marker_converges_while_writer_drains_queued_output() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
        let resize_slot = Arc::new(Mutex::new((24, 80, 0, 0)));
        outbound_tx
            .try_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"ahead")))
            .expect("unrelated output queued");
        *resize_slot.lock().expect("resize slot lock") = (30, 100, 9, 18);
        assert!(matches!(
            outbound_tx.try_send(ProxyOutbound::SyncResize),
            Err(tokio::sync::mpsc::error::TrySendError::Full(
                ProxyOutbound::SyncResize
            ))
        ));
        drop(outbound_tx);

        run_control_writer(
            Box::new(FakeWriter {
                output: Arc::clone(&output),
                write_error: None,
                write_gate: None,
            }),
            outbound_rx,
            resize_slot,
            Some((24, 80, 0, 0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(())),
            Arc::new(AtomicBool::new(false)),
            tokio::sync::mpsc::channel(1).0,
            "writer-test".into(),
            test_termination(
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ),
        );

        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[0], ClientMessage::Input { .. }));
        assert!(matches!(
            &messages[0],
            ClientMessage::Input { data } if data == b"ahead"
        ));
        assert!(matches!(
            messages[1],
            ClientMessage::Resize {
                cols: 100,
                rows: 30,
                cell_width_px: 9,
                cell_height_px: 18,
            }
        ));
        assert!(matches!(messages[2], ClientMessage::Detach));
    }

    #[test]
    fn poisoned_resize_slot_keeps_real_geometry_in_hello() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let resize_slot = Arc::new(Mutex::new((30, 100, 9, 18)));
        let poison_slot = Arc::clone(&resize_slot);
        assert!(std::thread::spawn(move || {
            let _guard = poison_slot.lock().expect("resize slot starts healthy");
            panic!("poison resize slot");
        })
        .join()
        .is_err());

        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(2);
        let channels = RemoteProxyChannels {
            outbound_rx,
            detach_tx: outbound_tx,
            resize_slot,
            input_enabled: Arc::new(AtomicBool::new(false)),
            operation_state: crate::remote::RemoteFocusOperationState::new(),
        };
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        let mut transport = transport_with(welcome_bytes(), Arc::clone(&output), None);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let _ = event_rx.blocking_recv();

        let messages = wire_messages(&output.lock().expect("fake output lock"));
        assert!(matches!(
            messages.first(),
            Some(ClientMessage::Hello {
                cols: 100,
                rows: 30,
                cell_width_px: 9,
                cell_height_px: 18,
                ..
            })
        ));
    }

    #[test]
    // Input, the latest resize, and detach reach the wire in order after the
    // control request.
    fn input_resize_and_detach_are_written_in_order() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        // Hold the reader at the stream's end so the outbound messages land
        // before the EOF the remote close would produce.
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport = transport_with_read_gate(
            input,
            Arc::clone(&output),
            None,
            Some(Arc::clone(&read_gate)),
        );
        let (channels, outbound_tx) = test_channels();
        let resize_slot = Arc::clone(&channels.resize_slot);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        outbound_tx
            .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"answer")))
            .expect("input queued");
        *resize_slot.lock().expect("resize slot lock") = (30, 100, 9, 18);
        outbound_tx
            .blocking_send(ProxyOutbound::SyncResize)
            .expect("resize queued");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if wire_messages(&output.lock().expect("fake output lock")).len() >= 4 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "input and resize did not reach the wire before detach"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        transport.detach("operation");
        read_gate.store(true, Ordering::Release);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let written = output.lock().expect("fake output lock");
                let messages = wire_messages(&written);
                if messages.len() >= 5 {
                    assert!(matches!(messages[0], ClientMessage::Hello { .. }));
                    assert!(matches!(messages[1], ClientMessage::ControlTerminal { .. }));
                    assert!(
                        matches!(&messages[2], ClientMessage::Input { data } if data == b"answer")
                    );
                    assert!(matches!(
                        messages[3],
                        ClientMessage::Resize {
                            cols: 100,
                            rows: 30,
                            cell_width_px: 9,
                            cell_height_px: 18,
                        }
                    ));
                    assert!(matches!(messages[4], ClientMessage::Detach));
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "input, resize, and detach did not reach the wire: {:?}",
                output.lock().expect("fake output lock").len()
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if transport.sessions.lock().expect("sessions lock").is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detach must unregister the session"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn detach_preempts_pending_outbound_input() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport = transport_with_read_gate(
            input,
            Arc::clone(&output),
            None,
            Some(Arc::clone(&read_gate)),
        );
        // Keep the stale input queued until detach has raised its urgent flag.
        let writer_start_gate = Arc::new(std::sync::Barrier::new(2));
        transport.writer_start_gate = Some(Arc::clone(&writer_start_gate));
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
        let channels = RemoteProxyChannels {
            outbound_rx,
            detach_tx: outbound_tx.clone(),
            resize_slot: Arc::new(Mutex::new((24, 80, 0, 0))),
            input_enabled: Arc::new(AtomicBool::new(false)),
            operation_state: crate::remote::RemoteFocusOperationState::new(),
        };
        outbound_tx
            .try_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"stale")))
            .expect("pending input queued");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while wire_messages(&output.lock().expect("fake output lock")).len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "control handshake did not reach the wire before detach"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        transport.detach("operation");
        writer_start_gate.wait();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let messages = wire_messages(&output.lock().expect("fake output lock"));
            if messages.len() >= 3 {
                assert!(matches!(messages[0], ClientMessage::Hello { .. }));
                assert!(matches!(messages[1], ClientMessage::ControlTerminal { .. }));
                assert!(matches!(messages[2], ClientMessage::Detach));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detach did not preempt pending input: {messages:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        read_gate.store(true, Ordering::Release);
    }

    #[test]
    // An intentional detach closes quietly: the stream ending afterwards is
    // not a connection loss and no failure reaches the app.
    fn detach_after_activation_is_quiet() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        // Hold the reader before the stream's end so the detach lands first,
        // then release it into the EOF the remote close would produce.
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport = transport_with_read_gate(
            input,
            Arc::clone(&output),
            None,
            Some(Arc::clone(&read_gate)),
        );
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        transport.detach("operation");
        read_gate.store(true, Ordering::Release);

        // The reader now sees the EOF the remote close produced; an
        // intentional detach must not surface as a failure. The session's
        // event sender drops when it ends, closing the channel quietly.
        let unexpected = event_rx.blocking_recv();
        assert!(
            unexpected.is_none(),
            "detach must not produce failure events: {unexpected:?}"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let written = output.lock().expect("fake output lock");
                let messages = wire_messages(&written);
                if messages
                    .last()
                    .is_some_and(|message| matches!(message, ClientMessage::Detach))
                {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detach did not reach the wire"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if transport.sessions.lock().expect("sessions lock").is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detach must unregister the session"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    // A panic inside the session thread must still run teardown: the app
    // hears a failure (closing the proxy operation) and the session
    // registration is removed instead of staying leased forever.
    fn a_session_panic_still_tears_down_registration_and_operation() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let panic_at = input.len() as u64;
        let mut transport = transport_with_panic_at(input, Arc::clone(&output), panic_at);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        let error = receive_failure(&mut event_rx);
        assert_eq!(error.code, "connection_lost");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if transport.sessions.lock().expect("sessions lock").is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the panic left the session registered"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
