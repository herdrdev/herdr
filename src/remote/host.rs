//! Remote-host side of the SSH stdio bridge.

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostState {
    Online,
    Stopped,
    NotRunning,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) struct HostStatus {
    pub(crate) epoch: u64,
    pub(crate) state: HostState,
}

impl HostStatus {
    pub(crate) fn inactive(self) -> bool {
        matches!(self.state, HostState::Stopped | HostState::NotRunning)
    }
}

impl std::fmt::Display for HostStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.state == HostState::Stopped {
            "Server stopped. Use Start to start it."
        } else {
            "Server is not running. Use Start to start it."
        })
    }
}
impl std::error::Error for HostStatus {}

pub(crate) fn host_status() -> io::Result<HostStatus> {
    let guard = crate::server::lifecycle::lock(&crate::api::socket_path())?;
    let client_listening = crate::server::autodetect::is_server_listening()?;
    let api_listening =
        crate::server::autodetect::is_server_listening_at(&crate::api::socket_path())?;
    let state = match (client_listening, api_listening, guard.receipt.stopped) {
        (true, true, _) => HostState::Online,
        (false, false, true) => HostState::Stopped,
        (false, false, false) => HostState::NotRunning,
        _ => HostState::Unavailable,
    };
    Ok(HostStatus {
        epoch: guard.receipt.epoch,
        state,
    })
}

pub(crate) fn start_host(epoch: u64) -> io::Result<()> {
    let status = host_status()?;
    if status.epoch != epoch {
        return Err(io::Error::other(
            "server state changed; refresh before starting",
        ));
    }
    match status.state {
        HostState::Online => {}
        HostState::Unavailable => {
            return Err(io::Error::other(
                "server is not ready; retry when startup or shutdown completes",
            ))
        }
        HostState::Stopped | HostState::NotRunning => {
            crate::server::autodetect::spawn_server_daemon_at_epoch(epoch)?;
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match host_status() {
            Ok(status) if status.state == HostState::Online => {
                if crate::api::read_runtime_status_at(
                    &crate::api::socket_path(),
                    Duration::from_millis(500),
                )?
                .is_some()
                {
                    return Ok(());
                }
            }
            Ok(status) if status.epoch != epoch && status.state == HostState::Stopped => {
                return Err(io::Error::other(
                    "server was stopped while starting; refresh before starting again",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "server start is unconfirmed; refresh its status before trying again",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn run_remote_client_bridge(args: &[String]) -> io::Result<()> {
    match args {
        [version, action] if version == "--host-control-v1" && action == "status" => {
            println!("{}", serde_json::to_string(&host_status()?)?);
            return Ok(());
        }
        [version, action, epoch] if version == "--host-control-v1" && action == "start" => {
            let epoch = epoch.parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid server start epoch")
            })?;
            return start_host(epoch);
        }
        _ => {}
    }
    let idle_timeout = match args {
        [] => false,
        [option]
            if option == "--idle-timeout-v1"
                && crate::platform::REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED =>
        {
            true
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported remote client bridge option",
            ))
        }
    };
    require_remote_server_running()?;
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

fn require_remote_server_running() -> io::Result<()> {
    if crate::server::autodetect::is_server_listening()? {
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

    Err(io::Error::new(
        io::ErrorKind::ConnectionRefused,
        "remote Herdr server is unavailable",
    ))
}
