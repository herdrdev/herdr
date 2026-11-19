//! Remote-host side of the SSH stdio bridge.

use std::io;
use std::time::Duration;

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RemoteStartStatus {
    Ready,
    WindowsSignInRequired,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct RemoteStartResponse {
    pub status: RemoteStartStatus,
}

pub(crate) fn run_remote_client_bridge(args: &[String]) -> io::Result<()> {
    let (idle_timeout, start_only, allow_headless) = match args {
        [] => (false, false, false),
        [option]
            if option == "--idle-timeout-v1"
                && crate::platform::REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED =>
        {
            (true, false, false)
        }
        [option] if option == "--start-only-v1" => (false, true, false),
        [option, force] if option == "--start-only-v1" && force == "--allow-headless-start-v1" => {
            (false, true, true)
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported remote client bridge option",
            ))
        }
    };
    let result = ensure_remote_server_running(allow_headless);
    if start_only {
        let response = RemoteStartResponse {
            status: remote_start_status(result)?,
        };
        println!(
            "{}",
            serde_json::to_string(&response).map_err(io::Error::other)?
        );
        return Ok(());
    }
    result?;
    #[cfg(unix)]
    let _ssh_agent = super::ssh_agent::Registration::start();

    let socket_path = crate::server::socket_paths::client_socket_path();
    let stream = crate::ipc::connect_local_stream(&socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to remote Herdr client socket {}: {err}",
                socket_path.display()
            ),
        )
    })?;

    crate::platform::forward_remote_bridge_stdio(stream, idle_timeout)
}

fn remote_start_status(result: io::Result<()>) -> io::Result<RemoteStartStatus> {
    match result {
        Ok(()) => Ok(RemoteStartStatus::Ready),
        Err(error) if crate::platform::is_windows_sign_in_required(&error) => {
            Ok(RemoteStartStatus::WindowsSignInRequired)
        }
        Err(error) => Err(error),
    }
}

fn ensure_remote_server_running(allow_headless: bool) -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if !crate::server::autodetect::is_server_listening()? {
        if allow_headless {
            crate::server::autodetect::spawn_remote_server_daemon_with_headless(true)?;
        } else {
            crate::server::autodetect::spawn_remote_server_daemon()?;
        }
        crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))?;
    }
    let status =
        crate::api::read_runtime_status_at(&crate::api::socket_path(), Duration::from_millis(500))?
            .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
    if status
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.endpoint_protocol_generation)
        == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
    {
        return Ok(());
    }
    Err(io::Error::other(
        "remote herdr server needs one final update before this bridge can attach; rerun `herdr --remote` from an interactive terminal to approve it",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_only_never_turns_other_launch_failures_into_waiting() {
        for kind in [
            io::ErrorKind::WouldBlock,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut,
        ] {
            let error = remote_start_status(Err(io::Error::new(kind, "unrelated startup failure")))
                .unwrap_err();
            assert_eq!(error.kind(), kind);
        }
        assert_eq!(
            remote_start_status(Ok(())).unwrap(),
            RemoteStartStatus::Ready
        );
        #[cfg(windows)]
        assert_eq!(
            remote_start_status(Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                crate::platform::WindowsSignInRequired
            )))
            .unwrap(),
            RemoteStartStatus::WindowsSignInRequired
        );
    }
}
