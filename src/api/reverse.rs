//! Explicit, machine-scoped Gram reverse gateway over a saved SSH connection.
//!
//! The coordinator never forwards its unrestricted API socket. A private socket
//! accepts only the seven Gram relay operations below, stamps the saved peer's
//! verified routing alias, and is reverse-forwarded to that peer by SSH. A local
//! process on either trusted machine can impersonate a pane of that machine;
//! this is not a per-process security boundary.

use interprocess::local_socket::{
    traits::{Listener, Stream},
    ListenerNonblockingMode,
};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::api::client::{ApiClient, ConnectionTarget};
use crate::api::federation_manager::PeerRoute;
use crate::api::gram_gateway::{GramGatewaySession, GramGatewaySpawner, GramGatewaySpec};
use crate::api::schema::{GramRelayCall, GramRelayParams, Method, Request};
use crate::api::ApiRequestSender;

const MAX_LINE: usize = 1_100_000; // one 512-KiB chunk encoded as JSON/base64
const RESPONSE_LIMIT: usize = 2_000_000;

pub(crate) fn forward_local(request: &Request) -> Option<String> {
    let method = &request.method;
    // Status is always answered by this daemon.
    if matches!(method, Method::GramRelayStatus(_)) {
        return None;
    }
    // Read per request so reload-config changes apply to the next call.
    let path = crate::api::gram_relay::policy().remote_socket()?;
    if !matches!(
        method,
        Method::GramSend(_)
            | Method::GramList(_)
            | Method::GramUploadChunk(_)
            | Method::GramUploadDiscard(_)
            | Method::GramGetFileChunk(_)
            | Method::GramDelete(_)
    ) {
        let name = crate::api::server::api_method_name(method);
        return name.starts_with("gram.").then(|| {
            serde_json::json!({"id":request.id,"error":{
                "code":"gram_relay_unsupported",
                "message":format!("{name} is unavailable through the restricted Gram relay; use the coordinator")
            }})
            .to_string()
        });
    }
    Some(send_to_relay(&path, request))
}

/// Send one Gram call to the coordinator, whatever method, for callers inside
/// this daemon (the guest gate). `None` when this daemon keeps its own Gram.
pub(crate) fn forward_relay(request: &Request) -> Option<String> {
    let path = crate::api::gram_relay::policy().remote_socket()?;
    Some(send_to_relay(&path, request))
}

fn send_to_relay(path: &std::path::Path, request: &Request) -> String {
    let client = ApiClient::for_target(ConnectionTarget::SocketPath(path.to_path_buf()));
    let reply =
        client.request_value_bounded(request, RESPONSE_LIMIT, Duration::from_secs(30), None);
    match reply {
        Ok(value) => value.to_string(),
        Err(error) => serde_json::json!({"id":request.id,"error":{
            "code":"gram_relay_unavailable", "message":format!("Gram relay unavailable: {error}")
        }})
        .to_string(),
    }
}

/// The transport is authenticated by the saved SSH host key and profile. Only
/// one alias is bound to each private gateway listener; request-supplied aliases
/// are never read. There is no TCP listener and no raw gram.* policy exception.
fn serve_one(
    mut conn: crate::ipc::LocalStream,
    alias: &str,
    tx: &ApiRequestSender,
    route: &PeerRoute,
) -> io::Result<()> {
    conn.set_recv_timeout(Some(Duration::from_secs(30)))?;
    conn.set_send_timeout(Some(Duration::from_secs(30)))?;
    let mut line = Vec::new();
    let mut reader = BufReader::new(&mut conn);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete Gram relay request",
            ));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if line.len() + consumed > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Gram relay request exceeds bound",
            ));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    drop(reader);
    let request: Request = serde_json::from_slice(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let id = request.id.clone();
    let result = if !route.identity_validated() {
        serde_json::json!({"id":id,"error":{"code":"federation_identity_unverified","message":"saved peer identity not pinned"}}).to_string()
    } else {
        let call = match request.method {
            Method::GramSend(params) => Some(GramRelayCall::Send(params)),
            Method::GramList(params) => Some(GramRelayCall::List(params)),
            Method::GramUploadChunk(params) => Some(GramRelayCall::UploadChunk(params)),
            Method::GramUploadDiscard(params) => Some(GramRelayCall::UploadDiscard(params)),
            Method::GramGetFileChunk(params) => Some(GramRelayCall::GetFileChunk(params)),
            Method::GramDelete(params) => Some(GramRelayCall::Delete(params)),
            // The relay envelope carries only a guest's post; the alias is
            // this gateway's, never the request's.
            Method::GramRelay(GramRelayParams {
                call: GramRelayCall::Post(params),
                ..
            }) => Some(GramRelayCall::Post(params)),
            _ => None,
        };
        match call {
            Some(call) => crate::api::server::handle_reverse_gram(
                Request { id: id.clone(), method: Method::GramRelay(GramRelayParams { peer_alias: alias.to_owned(), call }) }, tx),
            None => serde_json::json!({"id":id,"error":{"code":"forbidden","message":"method unavailable on Gram relay"}}).to_string(),
        }
    };
    if result.len() > RESPONSE_LIMIT {
        let too_large = serde_json::json!({"id":id,"error":{
            "code":"gram_relay_response_too_large",
            "message":"Gram relay response exceeds bound; request a smaller list page"
        }})
        .to_string();
        conn.write_all(too_large.as_bytes())?;
        return conn.write_all(b"\n");
    }
    conn.write_all(result.as_bytes())?;
    conn.write_all(b"\n")
}

const FORWARD_READY_WINDOW: Duration = Duration::from_secs(2);
const FORWARD_READY_POLL: Duration = Duration::from_millis(100);

/// Owns a starting `ssh -R` child and kills and reaps it on drop, so no early
/// return during setup can leak a forward the supervisor would then duplicate.
struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    fn child(&mut self) -> &mut std::process::Child {
        self.0
            .as_mut()
            .expect("guarded child is present until released")
    }

    fn release(mut self) -> std::process::Child {
        self.0.take().expect("guarded child is released once")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// ExitOnForwardFailure makes a failed remote bind terminate SSH, so a forward
/// that survives `window` is established. Every failure path (exit, cancel, or
/// a `try_wait` error) drops the guard, which kills and reaps the child.
fn await_forward_ready(
    mut guard: ChildGuard,
    window: Duration,
    cancelled: impl Fn() -> bool,
) -> io::Result<std::process::Child> {
    let deadline = std::time::Instant::now() + window;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(FORWARD_READY_POLL.min(window));
        if cancelled() {
            return Err(io::Error::other("Gram relay gateway start cancelled"));
        }
        if let Some(status) = guard.child().try_wait()? {
            return Err(io::Error::other(format!(
                "Gram relay SSH reverse bind failed: {status}"
            )));
        }
    }
    Ok(guard.release())
}

/// Production spawner: one [`ReverseGateway`] attempt per call.
pub(crate) struct SshGramGatewaySpawner {
    tx: ApiRequestSender,
    running: Arc<AtomicBool>,
}

impl SshGramGatewaySpawner {
    pub(crate) fn new(tx: ApiRequestSender, running: Arc<AtomicBool>) -> Self {
        Self { tx, running }
    }
}

impl GramGatewaySpawner for SshGramGatewaySpawner {
    fn start(
        &self,
        spec: &GramGatewaySpec,
        route: &PeerRoute,
        cancel: &Arc<AtomicBool>,
    ) -> io::Result<Box<dyn GramGatewaySession>> {
        ReverseGateway::start(
            spec,
            route.clone(),
            self.tx.clone(),
            Arc::clone(&self.running),
            Arc::clone(cancel),
        )
        .map(|gateway| Box::new(gateway) as Box<dyn GramGatewaySession>)
    }
}

/// One gateway attempt: a private local listener plus one `ssh -R` child. It
/// never respawns SSH itself; [`crate::api::gram_gateway::GramRelaySupervisor`]
/// retries it after [`GramGatewaySession::exited`] reports a reason.
pub(crate) struct ReverseGateway {
    stop: Arc<AtomicBool>,
    exited: Arc<Mutex<Option<String>>>,
    listener: Option<JoinHandle<()>>,
    ssh: Option<JoinHandle<()>>,
    socket: PathBuf,
    socket_identity: crate::ipc::SocketFileIdentity,
}

impl GramGatewaySession for ReverseGateway {
    fn exited(&self) -> Option<String> {
        self.exited
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

fn record_exit(exited: &Mutex<Option<String>>, reason: String) {
    exited
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_or_insert(reason);
}

impl ReverseGateway {
    fn start(
        spec: &GramGatewaySpec,
        route: PeerRoute,
        tx: ApiRequestSender,
        running: Arc<AtomicBool>,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let profile_id = spec.profile_id.as_str();
        let ssh_target = spec
            .endpoint
            .strip_prefix("ssh://")
            .ok_or_else(|| io::Error::other("Gram reverse gateway requires an SSH peer"))?;
        let coordinator_id = crate::persist::machine::get_or_create();
        let remote =
            crate::api::gram_relay::reverse_socket_path(&coordinator_id, &spec.remote_machine_id);
        let socket = crate::platform::remote_bridge_endpoint_path(
            &format!(
                "herdr-gram-gateway-{}-{}.sock",
                std::process::id(),
                profile_id
            ),
            &format!(
                "hg-{}-{}.sock",
                std::process::id(),
                profile_id.get(..16).unwrap_or(profile_id)
            ),
        );
        let listener = crate::ipc::bind_private_local_listener(&socket)?;
        let socket_identity = crate::ipc::socket_file_identity(&socket)?;
        crate::ipc::restrict_socket_permissions(&socket, 0o600)?;
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;
        // ExitOnForwardFailure makes a failed remote bind terminate SSH. Do not
        // advertise a ready gateway until the forwarding attempt has survived
        // its setup interval. The request path still fails closed until the
        // route's pinned machine identity is validated by federation polling.
        let child = (|| -> io::Result<std::process::Child> {
            let mut command = crate::remote::reverse_forward_command(
                profile_id,
                ssh_target,
                &spec.session,
                &remote,
                &socket,
                Arc::clone(&cancel),
            )?;
            await_forward_ready(
                ChildGuard::new(command.spawn()?),
                FORWARD_READY_WINDOW,
                || cancel.load(Ordering::Acquire) || !running.load(Ordering::Acquire),
            )
        })();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = crate::ipc::remove_socket_file_if_owned(&socket, &socket_identity);
                return Err(error);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(Mutex::new(None));
        let listener_stop = Arc::clone(&stop);
        let listener_running = Arc::clone(&running);
        let listener_exited = Arc::clone(&exited);
        let alias = spec.alias.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let mut last_sweep = std::time::Instant::now() - Duration::from_secs(60 * 60);
        let listener_thread = std::thread::spawn(move || {
            while !listener_stop.load(Ordering::Relaxed) && listener_running.load(Ordering::Relaxed)
            {
                if last_sweep.elapsed() >= Duration::from_secs(60 * 60) {
                    crate::persist::gram_files::sweep_stale_uploads();
                    last_sweep = std::time::Instant::now();
                }
                match listener.accept() {
                    Ok(conn) => {
                        if active.fetch_add(1, Ordering::AcqRel) >= 8 {
                            active.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        let active = Arc::clone(&active);
                        let alias = alias.clone();
                        let tx = tx.clone();
                        let route = route.clone();
                        std::thread::spawn(move || {
                            let _ = serve_one(conn, &alias, &tx, &route);
                            active.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25))
                    }
                    Err(error) => {
                        record_exit(
                            &listener_exited,
                            format!("Gram relay gateway listener failed: {error}"),
                        );
                        break;
                    }
                }
            }
        });
        let ssh_stop = Arc::clone(&stop);
        let ssh_exited = Arc::clone(&exited);
        let ssh_thread = std::thread::spawn(move || {
            while !ssh_stop.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        record_exit(
                            &ssh_exited,
                            format!("Gram relay SSH forward exited: {status}"),
                        );
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(250)),
                    Err(error) => {
                        record_exit(
                            &ssh_exited,
                            format!("Gram relay SSH forward failed: {error}"),
                        );
                        break;
                    }
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });
        Ok(Self {
            stop,
            exited,
            listener: Some(listener_thread),
            ssh: Some(ssh_thread),
            socket,
            socket_identity,
        })
    }
}

impl Drop for ReverseGateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(ssh) = self.ssh.take() {
            let _ = ssh.join();
        }
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        let _ = crate::ipc::remove_socket_file_if_owned(&self.socket, &self.socket_identity);
    }
}

/// A coordinator's gateway for one peer on `socket`, without SSH: each
/// connection is served by the production `serve_one` against `tx`.
#[cfg(test)]
pub(crate) fn serve_test_gateway(
    socket: &std::path::Path,
    alias: &str,
    tx: ApiRequestSender,
) -> JoinHandle<()> {
    let listener = crate::ipc::bind_private_local_listener(socket).unwrap();
    let route = PeerRoute::for_test(ConnectionTarget::SocketPath(socket.to_path_buf()));
    let alias = alias.to_string();
    std::thread::spawn(move || {
        while let Ok(conn) = listener.accept() {
            let (alias, tx, route) = (alias.clone(), tx.clone(), route.clone());
            std::thread::spawn(move || {
                let _ = serve_one(conn, &alias, &tx, &route);
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn sleeper() -> (ChildGuard, libc::pid_t) {
        let child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        (ChildGuard::new(child), pid)
    }

    /// True once the pid is gone entirely: killed AND reaped (no zombie).
    fn reaped(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks for the process's existence.
        let missing = unsafe { libc::kill(pid, 0) } == -1;
        missing && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[test]
    fn dropping_the_guard_kills_and_reaps_the_forward() {
        let (guard, pid) = sleeper();
        assert!(!reaped(pid));
        drop(guard);
        assert!(reaped(pid));
    }

    #[test]
    fn cancelled_readiness_kills_and_reaps_the_forward() {
        let (guard, pid) = sleeper();
        let error = await_forward_ready(guard, Duration::from_secs(5), || true).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(reaped(pid));
    }

    #[test]
    fn surviving_the_window_hands_over_the_live_child() {
        let (guard, pid) = sleeper();
        let mut child = await_forward_ready(guard, Duration::from_millis(250), || false).unwrap();
        assert!(!reaped(pid));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn an_exited_forward_reports_its_status() {
        let child = Command::new("sh").args(["-c", "exit 255"]).spawn().unwrap();
        let error = await_forward_ready(ChildGuard::new(child), Duration::from_secs(5), || false)
            .unwrap_err();
        assert!(error.to_string().contains("reverse bind failed"), "{error}");
    }
}
