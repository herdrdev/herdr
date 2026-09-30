mod control;
#[cfg(unix)]
mod host_unix;
mod machine_args;
mod machine_attach;
mod machine_process;
mod machine_restart_policy;
mod machine_saved;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc, Mutex, MutexGuard,
};
use tokio::sync::Notify;

#[cfg(test)]
#[derive(Clone)]
struct InputSendHook(Arc<dyn Fn() + Send + Sync>);

#[cfg(test)]
impl std::fmt::Debug for InputSendHook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InputSendHook(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum RemoteFocusOperationPhase {
    Live = 0,
    Terminal = 1,
}

/// Shared lifecycle state for one remote focus operation.
///
/// The transport changes this state before publishing a failure event, while
/// the app uses it to reject late frames and context transitions that were
/// already queued before the connection ended.
#[derive(Debug)]
pub(crate) struct RemoteFocusOperationState {
    phase: AtomicU8,
    input_admission: Mutex<()>,
    terminal_notify: Notify,
    #[cfg(test)]
    input_send_hook: Mutex<Option<InputSendHook>>,
}

impl RemoteFocusOperationState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            phase: AtomicU8::new(RemoteFocusOperationPhase::Live as u8),
            input_admission: Mutex::new(()),
            terminal_notify: Notify::new(),
            #[cfg(test)]
            input_send_hook: Mutex::new(None),
        })
    }

    pub(crate) fn terminate(&self) -> bool {
        let _admission = self
            .input_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let terminated = self
            .phase
            .compare_exchange(
                RemoteFocusOperationPhase::Live as u8,
                RemoteFocusOperationPhase::Terminal as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        drop(_admission);
        if terminated {
            self.terminal_notify.notify_waiters();
        }
        terminated
    }

    pub(crate) fn lock_input_admission(&self) -> MutexGuard<'_, ()> {
        self.input_admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn terminal_notification(&self) -> &Notify {
        &self.terminal_notify
    }

    #[cfg(test)]
    pub(crate) fn set_input_send_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self
            .input_send_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(InputSendHook(hook));
    }

    pub(crate) fn before_input_send(&self) {
        #[cfg(test)]
        if let Some(hook) = self
            .input_send_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            (hook.0)();
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.phase.load(Ordering::Acquire) == RemoteFocusOperationPhase::Terminal as u8
    }
}

pub(crate) use control::SshRemoteFocusTransport;
pub(crate) use machine_args::*;
pub(crate) use machine_attach::*;
pub(crate) use machine_saved::*;

pub(crate) const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;
const SURFACE_INTEREST_CAPABILITY: &str = "surface_interest";
const PRESENTATION_EFFECTS_FENCE_CAPABILITY: &str = "presentation_effects_fence";
const HEALTH_CHECK_CAPABILITY: &str = "health_check";
#[cfg(all(test, unix))]
// Test-only transport seams are re-exported for the real socket handshake harness.
pub(crate) use control::{ControlReadHalf, ControlStream, SshRunner, TimedRead};
#[cfg(unix)]
pub(crate) use host_unix::{run_remote_client_bridge, run_remote_control_bridge};

#[cfg(windows)]
pub(crate) fn run_remote_client_bridge() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "remote Windows hosts are not supported yet",
    ))
}

#[cfg(windows)]
pub(crate) fn run_remote_control_bridge() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "remote Windows hosts are not supported yet",
    ))
}

pub(crate) fn print_remote_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_auth_error(err) {
        eprintln!(
            "hint: verify SSH access first with `{}`.",
            ssh_check_command(target)
        );
        eprintln!(
            "hint: if your SSH key has a passphrase, load it into ssh-agent with `ssh-add` before running `herdr --remote`."
        );
    }
}

pub(crate) fn run_remote_api_bridge(args: &[String]) -> std::io::Result<()> {
    match args {
        [] => {
            let path = crate::api::socket_path();
            let stream = crate::ipc::connect_local_stream(&path).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "failed to connect to remote Herdr API socket {}: {error}",
                        path.display()
                    ),
                )
            })?;
            crate::platform::forward_remote_bridge_stdio(stream, false)
        }
        [flag] if flag == "--check" => {
            println!("herdr-api-bridge-v1");
            Ok(())
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: herdr remote-api-bridge [--check]",
        )),
    }
}

pub(crate) fn print_saved_ssh_error_hint(err: &std::io::Error, target: &str) {
    let message = err.to_string().to_ascii_lowercase();
    if message.contains("host key verification failed")
        || message.contains("remote host identification has changed")
    {
        eprintln!(
            "hint: saved machines use strict host-key checking; add the host key to the configured known_hosts file, then retry."
        );
    } else {
        print_remote_error_hint(err, target);
    }
}

fn is_remote_auth_error(err: &std::io::Error) -> bool {
    let message = err.to_string();
    message.contains("Permission denied")
        && (message.contains("(publickey")
            || message.contains("(keyboard-interactive")
            || message.contains("(password"))
}

fn ssh_check_command(target: &str) -> String {
    format!("ssh {}", shell_quote(target))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_auth_error_matches_ssh_auth_denied() {
        let err = std::io::Error::other(
            "remote platform detection failed: user@host: Permission denied (publickey).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_matches_keyboard_interactive_denied() {
        let err = std::io::Error::other(
            "remote server status failed: user@host: Permission denied (keyboard-interactive).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_ignores_non_auth_errors() {
        let err = std::io::Error::other("remote platform detection failed: unsupported platform");

        assert!(!is_remote_auth_error(&err));
    }

    #[test]
    fn ssh_check_command_quotes_remote_target() {
        assert_eq!(ssh_check_command("host name"), "ssh 'host name'");
    }
}
