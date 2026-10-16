//! Per-target OpenSSH transport. All sessions share a private master socket;
//! failures are retried on demand rather than by an unmanaged background loop.
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const STALE_AFTER: Duration = Duration::from_secs(3);
const FIRST_RETRY: Duration = Duration::from_millis(250);
const MAX_RETRY: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Reconnect {
    disconnected_at: Option<Instant>,
    retry_at: Option<Instant>,
    failures: u32,
}

impl Reconnect {
    fn failed(&mut self, now: Instant) {
        self.disconnected_at.get_or_insert(now);
        let delay = FIRST_RETRY
            .saturating_mul(1_u32.checked_shl(self.failures).unwrap_or(u32::MAX))
            .min(MAX_RETRY);
        self.retry_at = Some(now + delay);
        self.failures = self.failures.saturating_add(1);
    }

    fn ready(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|retry| now >= retry)
    }

    fn stale(&self, now: Instant) -> bool {
        self.disconnected_at
            .is_some_and(|since| now.saturating_duration_since(since) >= STALE_AFTER)
    }
}

struct State {
    connected: bool,
    reconnect: Reconnect,
    evidence: HashMap<String, super::HostEvidence>,
}

/// One supervisor per SSH target, independent of API socket/session selection.
/// `command` also supports future long-lived streams through the same master.
pub(crate) struct HostChannel {
    target: String,
    program: OsString,
    directory: PathBuf,
    state: Mutex<State>,
}

impl HostChannel {
    fn new(target: &str, program: OsString, base: &std::path::Path) -> Result<Self, String> {
        if target.is_empty() || target.starts_with('-') {
            return Err("invalid SSH target".into());
        }
        std::fs::create_dir_all(base).map_err(|error| error.to_string())?;
        // Atomic mkdir refuses collisions and symlinks; sockets live in a 0700
        // child even when XDG_STATE_HOME's ancestors are shared.
        let directory = (0..1000)
            .find_map(|attempt| {
                let path = base.join(format!("ssh-{}-{attempt}", std::process::id()));
                match crate::platform::create_remote_private_dir(&path) {
                    Ok(()) => Some(Ok(path)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error.to_string())),
                }
            })
            .ok_or_else(|| "cannot allocate fleet SSH directory".to_string())??;
        // OpenSSH appends a temporary suffix while binding the socket.
        if directory.join("control").as_os_str().len() > 80 {
            let _ = std::fs::remove_dir(&directory);
            return Err("fleet SSH control socket path exceeds Unix socket limit".into());
        }
        Ok(Self {
            target: target.into(),
            program,
            directory,
            state: Mutex::new(State {
                connected: false,
                reconnect: Reconnect::default(),
                evidence: HashMap::new(),
            }),
        })
    }

    fn base_command(&self, timeout: Duration) -> Command {
        let mut command = crate::noninteractive_process::command(&self.program);
        crate::remote::multiplex::apply_with_persist(
            &mut command,
            &self.directory.join("control"),
            "60",
        );
        command
            .args(["-o", "BatchMode=yes", "-o", "ConnectionAttempts=1", "-o"])
            .arg(format!("ConnectTimeout={}", timeout.as_secs().max(1)))
            .args(["-o", "ServerAliveInterval=2", "-o", "ServerAliveCountMax=1"]);
        command
    }

    fn output(
        &self,
        command: Command,
        input: Vec<u8>,
        deadline: Instant,
    ) -> std::io::Result<Output> {
        crate::noninteractive_process::output_with_stdin_and_deadline_limited(
            command,
            input,
            deadline,
            super::MAX_REMOTE_OUTPUT_BYTES,
        )
    }

    fn ensure_connected(&self, deadline: Instant) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        if !state.reconnect.ready(now) {
            return Err("STATUS UNKNOWN: SSH reconnect backoff".into());
        }
        if state.connected {
            // Socket disappearance is cheap to detect; a dead master leaving
            // its socket behind is detected by the fail-closed session exec.
            if self.directory.join("control").exists() {
                return Ok(());
            }
            state.connected = false;
            state.reconnect.failed(Instant::now());
            return Err("STATUS UNKNOWN: SSH master disconnected".into());
        }
        let mut master = self.base_command(deadline.saturating_duration_since(now));
        master.args(["-fN"]).arg(&self.target);
        match self.output(master, Vec::new(), deadline) {
            Ok(output) if output.status.success() => {
                state.connected = true;
                // Keep the outage start until a real request succeeds.
                state.reconnect.retry_at = None;
                Ok(())
            }
            result => {
                state.reconnect.failed(Instant::now());
                let detail = match result {
                    Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_string(),
                    Err(error) => error.to_string(),
                };
                tracing::debug!(target = %self.target, %detail, "fleet SSH reconnect failed");
                Err(format!("STATUS UNKNOWN: SSH master: {detail}"))
            }
        }
    }

    /// Build an arbitrary SSH session over this host's supervised master.
    /// A vanished socket fails closed instead of silently starting a handshake.
    pub(crate) fn command(&self, timeout: Duration) -> Result<Command, String> {
        self.ensure_connected(Instant::now() + timeout)?;
        let mut command = self.base_command(timeout);
        command.args(["-o", "ProxyCommand=false"]).arg(&self.target);
        Ok(command)
    }

    fn run(&self, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + timeout;
        let mut command = self.command(timeout)?;
        command.args(["sh", "-s"]);
        let result = self.output(command, script.as_bytes().to_vec(), deadline);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        match result {
            Ok(output) if output.status.success() => {
                state.reconnect = Reconnect::default();
                Ok(output.stdout)
            }
            result => {
                let transport_failed = match &result {
                    Ok(output) => output.status.code() == Some(255),
                    Err(error) => error.kind() != std::io::ErrorKind::FileTooLarge,
                };
                if transport_failed && state.connected {
                    state.connected = false;
                    state.reconnect.failed(Instant::now());
                }
                Err(match result {
                    Ok(output) => format!(
                        "STATUS UNKNOWN: ssh exited with {}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => format!(
                        "STATUS UNKNOWN: host read exceeded {} bytes",
                        super::MAX_REMOTE_OUTPUT_BYTES
                    ),
                    Err(error) => format!("STATUS UNKNOWN: ssh transport: {error}"),
                })
            }
        }
    }
}

impl Drop for HostChannel {
    fn drop(&mut self) {
        let mut command = self.base_command(Duration::from_secs(1));
        command.args(["-O", "exit"]).arg(&self.target);
        let _ = self.output(command, Vec::new(), Instant::now() + Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Owned by the CLI watch or fleet poller. Dropping the owner releases idle
/// supervisors; in-flight commands retain their channel until they finish.
pub(super) struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(channels) = CHANNELS.get() {
            let old = std::mem::take(&mut *channels.lock().unwrap_or_else(|e| e.into_inner()));
            drop(old);
        }
    }
}

type Channels = Mutex<HashMap<String, Arc<HostChannel>>>;
static CHANNELS: OnceLock<Channels> = OnceLock::new();

pub(crate) fn for_host(target: &str) -> Result<Arc<HostChannel>, String> {
    let mut channels = CHANNELS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(channel) = channels.get(target) {
        return Ok(Arc::clone(channel));
    }
    let channel = Arc::new(HostChannel::new(
        target,
        "ssh".into(),
        &crate::config::state_dir().join("fleet"),
    )?);
    channels.insert(target.into(), Arc::clone(&channel));
    Ok(channel)
}

pub(super) fn run(target: &str, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    for_host(target)?.run(script, timeout)
}

pub(super) fn retain_during_reconnect(evidence: super::HostEvidence) -> super::HostEvidence {
    let Ok(channel) = for_host(&evidence.host.target) else {
        return evidence;
    };
    let mut state = channel
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    retain(&mut state, evidence, Instant::now())
}

fn retain(state: &mut State, evidence: super::HostEvidence, now: Instant) -> super::HostEvidence {
    let key = format!("{:?}", evidence.host);
    if evidence.agents.is_ok() {
        state.evidence.insert(key, evidence.clone());
    } else if state.reconnect.disconnected_at.is_some() && !state.reconnect.stale(now) {
        return state.evidence.get(&key).cloned().unwrap_or(evidence);
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_schedule_and_stale_threshold_use_injected_clock() {
        let start = Instant::now();
        let mut state = Reconnect::default();
        let mut now = start;
        for delay_ms in [250, 500, 1000, 2000, 4000, 8000, 10000, 10000] {
            state.failed(now);
            let retry = now + Duration::from_millis(delay_ms);
            assert!(!state.ready(retry - Duration::from_nanos(1)));
            assert!(state.ready(retry));
            now = retry;
        }
        assert!(!state.stale(start + Duration::from_millis(2999)));
        assert!(state.stale(start + Duration::from_secs(3)));
        assert_eq!(state.disconnected_at, Some(start));
        state = Reconnect::default();
        assert!(state.ready(now));
        assert!(!state.stale(now));
    }

    #[test]
    fn host_snapshot_survives_only_the_three_second_reconnect_grace() {
        let now = Instant::now();
        let host = super::super::FleetHostConfig {
            name: "remote".into(),
            target: "remote".into(),
            ..Default::default()
        };
        let good = super::super::HostEvidence {
            host,
            agents: Ok(Vec::new()),
            groups: None,
            runs: Vec::new(),
            runtime: Default::default(),
            sessions: None,
            aloop: None,
            inbox: None,
        };
        let mut state = State {
            connected: true,
            reconnect: Reconnect::default(),
            evidence: HashMap::new(),
        };
        retain(&mut state, good.clone(), now);
        state.reconnect.failed(now);
        let mut failed = good;
        failed.agents = Err("disconnected".into());
        assert!(retain(
            &mut state,
            failed.clone(),
            now + Duration::from_millis(2999)
        )
        .agents
        .is_ok());
        assert!(retain(&mut state, failed, now + Duration::from_secs(3))
            .agents
            .is_err());
    }

    #[test]
    fn five_ticks_spawn_one_master_per_host_and_share_commands() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::path::PathBuf::from("/tmp").join(format!(
            "hc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        crate::platform::create_remote_private_dir(&root).expect("fixture directory");
        let program = root.join("ssh");
        std::fs::write(
            &program,
            r#"#!/bin/sh
socket=''
operation=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -S) socket="$2"; shift 2 ;;
    -O) operation="$2"; shift 2 ;;
    -o) shift 2 ;;
    -fN) operation=start; shift ;;
    *) shift ;;
  esac
done
case "$operation" in
  start) echo master >> "${socket}.starts"; touch "$socket" ;;
  check) test -f "$socket" ;;
  exit) rm -f "$socket" ;;
  *) test -f "$socket" || exit 255; cat ;;
esac
"#,
        )
        .expect("fake ssh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let a = HostChannel::new("a", program.clone().into(), &root).expect("a");
        let b = HostChannel::new("b", program.into(), &root).expect("b");
        for _ in 0..5 {
            for host in [&a, &b] {
                assert_eq!(
                    host.run("tick", Duration::from_secs(2)).expect("tick"),
                    b"tick"
                );
            }
        }
        for host in [&a, &b] {
            assert_eq!(
                std::fs::read_to_string(host.directory.join("control.starts")).expect("starts"),
                "master\n"
            );
            assert!(host.command(Duration::from_secs(1)).is_ok());
        }
        let oversized = "x".repeat(super::super::MAX_REMOTE_OUTPUT_BYTES + 1);
        assert!(a
            .run(&oversized, Duration::from_secs(5))
            .expect_err("output limit")
            .contains("exceeded"));
        assert!(a.state.lock().expect("state").connected);
        assert_eq!(
            a.run("still warm", Duration::from_secs(2))
                .expect("after output limit"),
            b"still warm"
        );
        assert_eq!(
            std::fs::read_to_string(a.directory.join("control.starts")).expect("starts"),
            "master\n"
        );
        // No sleeps: losing the socket records an outage and suppresses a
        // second spawn until the injected retry deadline is advanced.
        std::fs::remove_file(a.directory.join("control")).expect("kill master");
        assert!(a.run("tick", Duration::from_secs(1)).is_err());
        assert!(a.run("tick", Duration::from_secs(1)).is_err());
        {
            let mut state = a.state.lock().expect("state");
            state.reconnect.retry_at = Some(Instant::now());
        }
        assert_eq!(
            a.run("recovered", Duration::from_secs(1))
                .expect("reconnect"),
            b"recovered"
        );
        assert_eq!(
            std::fs::read_to_string(a.directory.join("control.starts")).expect("starts"),
            "master\nmaster\n"
        );
        assert!(a
            .state
            .lock()
            .expect("state")
            .reconnect
            .disconnected_at
            .is_none());
        let a_dir = a.directory.clone();
        drop(a);
        drop(b);
        assert!(!a_dir.exists());
        std::fs::remove_dir_all(&root).expect("cleanup fixture");
    }
}
