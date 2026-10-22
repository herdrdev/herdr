#[cfg(unix)]
mod api_bridge_unix;
mod args;
mod attach;
mod host;
mod process;
mod restart_policy;
mod saved;
#[cfg(unix)]
mod ssh_agent;

#[cfg(unix)]
pub(crate) use api_bridge_unix::run_api_client_bridge;
pub(crate) use args::*;
pub(crate) use attach::*;
pub(crate) use host::run_remote_client_bridge;
pub(crate) use saved::*;

/// Requests one `api-bridge --multi` bridge forwards at once. Each holds a
/// worker thread and a local socket connection; with every worker busy the
/// bridge stops reading stdin, so a burst back-pressures the SSH channel
/// instead of growing threads or buffers. A coordinator's persistent peer
/// channel keeps no more than this many requests outstanding on one channel.
pub(crate) const API_BRIDGE_MULTI_MAX_IN_FLIGHT: usize = 16;

/// Longest request line `api-bridge --multi` accepts, excluding its newline.
/// It matches the server's own initial-request cap, which would refuse
/// anything longer. Longer lines are drained and answered with
/// `invalid_request`.
#[cfg(unix)]
pub(crate) const API_BRIDGE_MULTI_MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;

/// Methods that keep their connection open after the first response line.
/// `api-bridge --multi` gives each request one connection and answers it from
/// that connection alone, so these keep their own SSH channel.
pub(crate) const API_BRIDGE_MULTI_STREAMING_METHODS: &[&str] = &[
    "events.subscribe",
    "pane.stream",
    "pane.input.stream",
    "gram.upload.stream",
    "server.ssh_agent.register",
];

#[cfg(windows)]
pub(crate) fn run_api_client_bridge(_args: &[String]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "api-bridge is only supported on Unix remote hosts",
    ))
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
    if is_remote_host_key_error(err) {
        eprintln!(
            "hint: saved machines use strict host-key checking; add the host key to the configured known_hosts file, then retry."
        );
    } else {
        print_remote_error_hint(err, target);
    }
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

fn is_remote_host_key_error(err: &std::io::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("host key verification failed")
        || message.contains("remote host identification has changed")
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

pub(crate) fn shell_quote(value: &str) -> String {
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
    fn remote_host_key_error_matches_ssh_diagnostics() {
        for message in [
            "Host key verification failed.",
            "REMOTE HOST IDENTIFICATION HAS CHANGED!",
        ] {
            assert!(is_remote_host_key_error(&std::io::Error::other(message)));
        }
        assert!(!is_remote_host_key_error(&std::io::Error::other(
            "server closed connection"
        )));
    }

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
