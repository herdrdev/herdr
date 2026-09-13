//! Remote-host side of the SSH stdio bridge.

use std::io;
use std::time::Duration;

pub(crate) fn run_remote_client_bridge(require_desktop: bool) -> io::Result<()> {
    ensure_remote_server_running(require_desktop)?;

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
    if require_desktop {
        crate::platform::verify_remote_desktop_stream(&stream)?;
    }

    crate::platform::forward_remote_bridge_stdio(stream)
}

fn ensure_remote_server_running(require_desktop: bool) -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if crate::server::autodetect::is_server_listening() {
        let status = crate::api::read_runtime_status_at(
            &crate::api::socket_path(),
            Duration::from_millis(500),
        )?
        .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
        if status
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.endpoint_protocol_generation)
            == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
        {
            return Ok(());
        }
        return Err(io::Error::other(
            "remote herdr server needs one final update before this bridge can attach; rerun `herdr --remote` from an interactive terminal to approve it",
        ));
    }

    if require_desktop {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the Windows desktop Herdr server is not running; run the interactive remote command to prepare it",
        ));
    }
    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))
}
