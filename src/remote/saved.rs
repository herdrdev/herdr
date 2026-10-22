use std::io;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
use std::time::{Duration, Instant};

use super::attach::{find_installed_remote_herdr, ManagedSshOptions, RemoteSsh, SshStdioBridge};

pub(crate) struct SavedSshBridge {
    _bridge: SshStdioBridge,
}

pub(crate) struct SavedSshStream {
    pub(crate) stream: crate::ipc::LocalStream,
    pub(crate) bridge: SavedSshBridge,
}

pub(crate) fn connect_saved_ssh(
    profile_id: &str,
    target: &str,
    session: &str,
    cancelled: Arc<AtomicBool>,
) -> io::Result<SavedSshStream> {
    let mut ssh = validated_saved_ssh(profile_id, target, session)?;
    ssh.cancelled = Arc::clone(&cancelled);
    ssh.prepare_shared_transport()?;
    let remote_herdr = find_installed_remote_herdr(&ssh)?;
    let metadata = remote_herdr.machine_metadata();
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let path = saved_bridge_path(profile_id);
    let bridge = SshStdioBridge::start(
        target.to_owned(),
        remote_herdr,
        path.clone(),
        session.to_owned(),
        ssh.options(),
        true,
        Arc::clone(&cancelled),
    )?;
    let stream = crate::ipc::connect_local_stream_cancellable(
        &path,
        &cancelled,
        Instant::now() + Duration::from_secs(5),
    )?;
    if let Some(metadata) = metadata {
        crate::client::endpoint::SshMetadataCache::new(profile_id, target, session)?
            .store(&metadata);
    }
    Ok(SavedSshStream {
        stream,
        bridge: SavedSshBridge { _bridge: bridge },
    })
}

pub(crate) struct SavedSshApiBridge {
    path: PathBuf,
    bridge: SshStdioBridge,
    metadata_cache: crate::client::endpoint::SshMetadataCache,
    ssh: RemoteSsh,
    pub(crate) can_retry: bool,
    pub(crate) used_cached_metadata: bool,
}

impl SavedSshApiBridge {
    pub(crate) fn start(
        profile_id: &str,
        target: &str,
        session: &str,
        use_cached_metadata: bool,
        allow_retry: bool,
    ) -> io::Result<Self> {
        Self::start_inner(
            profile_id,
            target,
            session,
            use_cached_metadata,
            allow_retry,
            Arc::new(AtomicBool::new(false)),
            None,
        )
    }

    /// Federation's supervised start: `cancellation` aborts discovery and the
    /// metadata probe. The bridge, discovery, and transport recovery share the
    /// process-wide federation SSH master that peer channels also use.
    /// Discovery is read-only, so one retry after recovery stays allowed.
    pub(crate) fn start_cancellable(
        profile_id: &str,
        target: &str,
        session: &str,
        use_cached_metadata: bool,
        cancellation: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        Self::start_inner(
            profile_id,
            target,
            session,
            use_cached_metadata,
            true,
            cancellation,
            saved_federation_ssh_options(),
        )
    }

    fn start_inner(
        profile_id: &str,
        target: &str,
        session: &str,
        use_cached_metadata: bool,
        allow_retry: bool,
        cancellation: Arc<AtomicBool>,
        transport: Option<&ManagedSshOptions>,
    ) -> io::Result<Self> {
        let mut ssh = validated_saved_ssh(profile_id, target, session)?;
        ssh.cancelled = cancellation;
        // One-shot commands keep the per-target managed master, whose stable
        // control path lets a later command reuse or retire it.
        if let Some(options) = transport {
            ssh.use_managed_options(options.clone());
        }
        let mut can_retry = allow_retry;
        let metadata_cache =
            crate::client::endpoint::SshMetadataCache::new(profile_id, target, session)?;
        let cached = use_cached_metadata.then(|| metadata_cache.load()).flatten();
        let used_cached_metadata = cached.is_some();
        let metadata = match cached {
            Some(metadata) => metadata,
            None => {
                let discover = || super::attach::discover_remote_api_metadata(&ssh, session);
                let metadata = match discover() {
                    Err(_) if can_retry && ssh.prepare_shared_transport().unwrap_or(false) => {
                        can_retry = false;
                        discover()?
                    }
                    result => result?,
                };
                metadata_cache.store(&metadata);
                metadata
            }
        };
        if ssh.cancelled.load(Ordering::Acquire) {
            return Err(super::process::cancelled());
        }
        let command = super::attach::cached_remote_api_command(&metadata, session);
        let path = crate::platform::remote_bridge_endpoint_path(
            &format!("herdr-api-ssh-{}-{profile_id}.sock", std::process::id()),
            &format!(
                "herdr-api-{}-{}.sock",
                std::process::id(),
                &profile_id[..16]
            ),
        );
        let bridge = SshStdioBridge::start_command(
            target.to_owned(),
            command,
            path.clone(),
            ssh.options(),
            true,
            Arc::new(AtomicBool::new(false)),
        )?;
        Ok(Self {
            path,
            bridge,
            metadata_cache,
            ssh,
            can_retry,
            used_cached_metadata,
        })
    }

    pub(crate) fn socket_path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn reported_failure(&self) -> Option<io::Error> {
        self.bridge.reported_failure()
    }

    pub(crate) fn try_reported_failure(&self) -> io::Result<Option<io::Error>> {
        self.bridge.try_reported_failure()
    }

    pub(crate) fn invalidate_metadata(&self) {
        self.metadata_cache.invalidate();
    }

    pub(crate) fn recover_transport(&self) -> bool {
        self.can_retry && self.ssh.prepare_shared_transport().unwrap_or(false)
    }

    pub(crate) fn stale_metadata_failure(error: &io::Error) -> bool {
        error
            .to_string()
            .contains(super::attach::STALE_API_METADATA)
    }
}

/// SSH command for a saved peer's persistent `api-bridge --multi` channel: the
/// same managed transport (and so the same ControlMaster) and cached remote
/// executable as [`SavedSshApiBridge`], for the same session.
pub(crate) fn saved_peer_api_channel_command(
    profile_id: &str,
    target: &str,
    session: &str,
) -> io::Result<std::process::Command> {
    let metadata = crate::client::endpoint::SshMetadataCache::new(profile_id, target, session)?
        .load()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "saved machine API metadata is not cached",
            )
        })?;
    let remote_command = super::attach::cached_remote_api_multi_command(&metadata, session)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "saved machine has no api-bridge --multi",
            )
        })?;
    Ok(super::attach::ssh_exec_command(
        target,
        &remote_command,
        saved_federation_ssh_options(),
        true,
    ))
}

/// A per-connection bridge running `remote_command` through `ssh` from `PATH`,
/// as a saved peer's bridge does, for tests with a fake `ssh`.
#[cfg(all(test, unix))]
pub(crate) fn start_test_ssh_bridge(
    target: &str,
    remote_command: &str,
    local_socket: PathBuf,
) -> io::Result<SavedSshBridge> {
    Ok(SavedSshBridge {
        _bridge: SshStdioBridge::start_command(
            target.to_owned(),
            remote_command.to_owned(),
            local_socket,
            None,
            true,
            Arc::new(AtomicBool::new(false)),
        )?,
    })
}

/// Dedicated reverse stream-local forward; NEVER point this at the unrestricted
/// coordinator API socket. The caller provides its restricted per-peer gateway.
#[cfg(unix)]
pub(crate) fn reverse_forward_command(
    profile_id: &str,
    target: &str,
    session: &str,
    remote_socket: &std::path::Path,
    local_gateway: &std::path::Path,
    cancellation: Arc<AtomicBool>,
) -> io::Result<std::process::Command> {
    let _ = validated_saved_ssh(profile_id, target, session)?;
    // sshd leaves a stream-local -R socket pathname behind after the forwarding
    // session exits. StreamLocalBindUnlink is client-side and does not remove
    // that remote pathname on ordinary sshd installations. Clear only the
    // socket reserved for this pinned machine pair before every (re)bind.
    let quoted = super::shell_quote(&remote_socket.to_string_lossy());
    let cleanup = format!(
        "if [ -e {quoted} ] || [ -L {quoted} ]; then\n  [ ! -L {quoted} ] && [ -S {quoted} ] || exit 1\n  rm -- {quoted}\nfi"
    );
    let preflight =
        super::attach::RemoteSsh::new_noninteractive_cancellable(target.to_owned(), cancellation)
            .sh_output(&cleanup)?;
    if !preflight.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "remote Gram reverse socket preflight failed: {}",
                preflight.status
            ),
        ));
    }
    let mut command = std::process::Command::new("ssh");
    // A persistent multiplexing master exits successfully after forking, which
    // makes the gateway treat a live -R forward as a failed startup and remove
    // its local listener. Keep this dedicated forward in the foreground.
    command.arg("-C").arg("-S").arg("none");
    if let Some(options) = saved_federation_ssh_options() {
        command.arg("-F").arg(&options.config_path);
    }
    command
        .arg("-o")
        .arg("ControlMaster=no")
        .arg("-o")
        .arg("ControlPersist=no");
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("StreamLocalBindMask=0177")
        .arg("-o")
        .arg("StreamLocalBindUnlink=yes")
        .arg("-N")
        .arg("-R")
        .arg(format!(
            "{}:{}",
            remote_socket.display(),
            local_gateway.display()
        ))
        .arg("--")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    Ok(command)
}

fn saved_federation_ssh_options() -> Option<&'static ManagedSshOptions> {
    static OPTIONS: OnceLock<Option<ManagedSshOptions>> = OnceLock::new();
    OPTIONS
        .get_or_init(|| {
            super::attach::build_federation_ssh_options()
                .inspect_err(|error| {
                    tracing::warn!(%error, "could not create saved federation SSH config; using plain SSH");
                })
                .ok()
        })
        .as_ref()
}

/// Establish an authenticated SSH streamlocal reverse forward, never a forward
/// to the unrestricted Herdr API. A readiness marker is emitted by the remote
/// command only after OpenSSH has accepted all requested forwards.
#[cfg(unix)]
pub(crate) fn spawn_saved_reverse_forward(
    profile_id: &str,
    target: &str,
    remote_socket: &std::path::Path,
    gateway_socket: &std::path::Path,
) -> io::Result<std::process::Child> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    validate_profile_path_id(profile_id)?;
    if target.is_empty() || target.starts_with('-') || target.chars().any(char::is_whitespace) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid saved SSH target",
        ));
    }
    let options = saved_federation_ssh_options().ok_or_else(|| {
        io::Error::other("managed SSH configuration unavailable; reverse forwarding refused")
    })?;
    // sshd retains the remote socket pathname after disconnect. Remove only
    // this socket (never a file or symlink) before binding the new forward.
    let quoted = super::shell_quote(&remote_socket.to_string_lossy());
    let cleanup = format!(
        "if [ -e {quoted} ] || [ -L {quoted} ]; then\n  [ ! -L {quoted} ] && [ -S {quoted} ] || exit 1\n  rm -- {quoted}\nfi"
    );
    let preflight =
        super::attach::RemoteSsh::new_noninteractive(target.to_owned()).sh_output(&cleanup)?;
    if !preflight.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "remote reverse socket preflight failed: {}",
                preflight.status
            ),
        ));
    }
    let mut command = Command::new("ssh");
    // A managed multiplexing master may return before the dedicated forward
    // closes. Keep this transport owned by the gateway for its entire lifetime.
    command
        .arg("-C")
        .arg("-S")
        .arg("none")
        .arg("-F")
        .arg(&options.config_path)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg("-o")
        .arg("ControlPersist=no");
    super::attach::apply_noninteractive_ssh_options(&mut command);
    command
        .arg("-T")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("StreamLocalBindMask=0177")
        .arg("-o")
        .arg("StreamLocalBindUnlink=yes")
        .arg("-R")
        .arg(format!(
            "{}:{}",
            remote_socket.display(),
            gateway_socket.display()
        ))
        .arg(target)
        .arg("printf 'herdr-reverse-ready\\n'; exec sleep 2147483647")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped SSH stdout");
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut output = BufReader::new(stdout);
        let mut line = Vec::new();
        let mut total = 0usize;
        let result = loop {
            let available = match output.fill_buf() {
                Ok(bytes) if !bytes.is_empty() => bytes,
                Ok(_) => {
                    break Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "SSH closed before reverse forward readiness",
                    ))
                }
                Err(error) => break Err(error),
            };
            let newline = available.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(available.len(), |position| position + 1);
            total += count;
            if total > 16 * 1024 {
                break Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SSH stdout exceeded readiness limit",
                ));
            }
            line.extend_from_slice(&available[..count]);
            output.consume(count);
            if newline.is_some() {
                if line == b"herdr-reverse-ready\n" {
                    break Ok(());
                }
                line.clear();
            }
        };
        let _ = tx.send(result);
    });
    let ready = rx.recv_timeout(Duration::from_secs(20));
    if !matches!(ready, Ok(Ok(()))) {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err(match ready {
            Err(mpsc::RecvTimeoutError::Timeout) => io::Error::new(
                io::ErrorKind::TimedOut,
                "SSH reverse streamlocal forwarding did not become ready within 20 seconds",
            ),
            Ok(Err(error)) => error,
            _ => io::Error::other("SSH reverse streamlocal forwarding rejected"),
        });
    }
    let _ = reader.join();
    Ok(child)
}

pub(crate) fn saved_ssh_bootstrap_command(target: &str, session: &str) -> String {
    format!(
        "herdr --remote {} --session {}",
        super::shell_quote(target),
        super::shell_quote(session)
    )
}

pub(crate) fn saved_ssh_failure_needs_attention(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput
            | io::ErrorKind::InvalidData
            | io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::Unsupported
    ) {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    [
        "permission denied",
        "host key verification failed",
        "remote host identification has changed",
        "could not resolve hostname",
        "no matching host key",
        "unsupported remote platform",
        "not ready",
        "install or update",
        "protocol",
        "handshake",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn saved_bridge_path(profile_id: &str) -> PathBuf {
    let pid = std::process::id();
    let readable = format!("herdr-ssh-{pid}-{profile_id}.sock");
    let short = format!("herdr-s-{pid}-{}.sock", &profile_id[..16]);
    crate::platform::remote_bridge_endpoint_path(&readable, &short)
}

fn validated_saved_ssh(profile_id: &str, target: &str, session: &str) -> io::Result<RemoteSsh> {
    validate_profile_path_id(profile_id)?;
    crate::session::validate_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(RemoteSsh::new_noninteractive(target.to_owned()))
}

fn validate_profile_path_id(profile_id: &str) -> io::Result<()> {
    if profile_id.len() == 32
        && profile_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH endpoint profile id",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_paths_use_profile_identity_not_target_or_session() {
        let first = saved_bridge_path("0123456789abcdef0123456789abcdef");
        let second = saved_bridge_path("fedcba9876543210fedcba9876543210");
        assert_ne!(first, second);
        assert!(!first.to_string_lossy().contains("example.com"));
        assert!(!first.to_string_lossy().contains("default"));
    }

    #[test]
    fn bootstrap_command_preserves_the_explicit_remote_session() {
        assert_eq!(
            saved_ssh_bootstrap_command("build host", "agent work"),
            "herdr --remote 'build host' --session 'agent work'"
        );
    }

    #[test]
    fn prompt_and_compatibility_failures_require_attention() {
        for message in [
            "Permission denied (publickey)",
            "Host key verification failed",
            "matching Herdr is not ready; install or update",
            "handshake rejected",
        ] {
            assert!(saved_ssh_failure_needs_attention(&io::Error::other(
                message
            )));
        }
        for message in [
            "ssh: connect to host build port 22: Connection timed out",
            "ssh: connect to host build port 22: Connection refused",
        ] {
            assert!(
                !saved_ssh_failure_needs_attention(&io::Error::other(message)),
                "transient reachability failures must remain retryable"
            );
        }
    }
}
