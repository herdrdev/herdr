use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::client::{parse_response_value, ApiClientError};
use crate::api::schema::{Method, Request, ResponseResult, ServerSshAgentRegisterParams};
use crate::ipc::{LocalStream, LocalStreamRead, LocalStreamReadCount};

pub(super) struct Registration {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Registration {
    pub(super) fn start() -> Option<Self> {
        let path = std::env::var("SSH_AUTH_SOCK")
            .ok()
            .filter(|path| !path.is_empty())?;
        Self::start_at(path, crate::api::socket_path())
    }

    fn start_at(path: String, socket_path: PathBuf) -> Option<Self> {
        let mut stream = match connect(&path, &socket_path) {
            Ok(None) => return None,
            Ok(stream) => stream,
            Err(error) => {
                tracing::debug!(%error, "SSH agent refresh unavailable; retrying while attached");
                None
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut byte = [0];
            while !worker_stop.load(Ordering::Relaxed) {
                if let Some(connection) = stream.as_mut() {
                    if !matches!(
                        crate::ipc::poll_local_stream_read(connection, &mut byte),
                        Ok(LocalStreamRead::Pending)
                    ) {
                        stream = None;
                    }
                } else {
                    // Retry both initial API readiness and connections lost during handoff.
                    match connect(&path, &socket_path) {
                        Ok(None) => break,
                        result => stream = result.ok().flatten(),
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Some(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn connect(path: &str, socket_path: &Path) -> io::Result<Option<LocalStream>> {
    let timeout = Duration::from_millis(500);
    let status = crate::api::read_runtime_status_at(socket_path, timeout)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "SSH agent status API is not ready",
        )
    })?;
    if !status
        .capabilities
        .is_some_and(|capabilities| capabilities.ssh_agent_registration)
    {
        return Ok(None);
    }
    let mut stream = crate::ipc::connect_local_stream(socket_path)?;
    let request = Request {
        id: "remote:ssh-agent".into(),
        method: Method::ServerSshAgentRegister(ServerSshAgentRegisterParams {
            socket_path: path.into(),
        }),
    };
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    crate::ipc::set_local_stream_polling(&mut stream, true)?;
    let deadline = Instant::now() + timeout;
    let mut response = Vec::new();
    let mut byte = [0];
    while Instant::now() < deadline && response.len() < 4096 {
        match crate::ipc::poll_local_stream_read_count(&mut stream, &mut byte)? {
            LocalStreamReadCount::Data(_) if byte[0] == b'\n' => {
                let response = match parse_response_value(serde_json::from_slice(&response)?) {
                    Ok(response) => response,
                    Err(ApiClientError::ErrorResponse(response))
                        if response.error.code == "invalid_ssh_agent" =>
                    {
                        tracing::debug!(error = %response.error.message, "SSH agent registration rejected");
                        return Ok(None);
                    }
                    Err(error) => return Err(io::Error::other(error)),
                };
                return match response.result {
                    ResponseResult::Ok {} => Ok(Some(stream)),
                    _ => Err(io::Error::other(
                        "unexpected SSH agent registration response",
                    )),
                };
            }
            LocalStreamReadCount::Data(_) => response.push(byte[0]),
            LocalStreamReadCount::Closed => {
                return Err(io::Error::other("SSH agent registration closed"))
            }
            LocalStreamReadCount::Pending => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "SSH agent registration did not complete",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::net::UnixListener;

    #[test]
    fn registration_stops_when_the_server_rejects_the_agent() {
        let socket_path =
            std::env::temp_dir().join(format!("shepherd-agent-rejected-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            for expected in ["ping", "server.ssh_agent.register"] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(&mut stream).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], expected);
                let response = if expected == "ping" {
                    serde_json::json!({"id": request["id"], "result": {
                        "type": "pong", "version": "test", "protocol": crate::protocol::PROTOCOL_VERSION,
                        "capabilities": {"live_handoff": false, "ssh_agent_registration": true}
                    }})
                } else {
                    serde_json::json!({"id": request["id"], "error": {
                        "code": "invalid_ssh_agent", "message": "invalid agent path"
                    }})
                };
                writeln!(stream, "{response}").unwrap();
            }
        });
        let registration =
            Registration::start_at("relative-agent.sock".into(), socket_path.clone());
        server.join().unwrap();
        std::fs::remove_file(socket_path).unwrap();
        assert!(
            registration.is_none(),
            "a rejected agent must not start a retry worker"
        );
    }

    #[test]
    fn registration_retries_when_the_api_is_initially_missing() {
        let socket_path =
            std::env::temp_dir().join(format!("shepherd-agent-retry-{}.sock", std::process::id()));
        let registration = Registration::start_at("/test/agent.sock".into(), socket_path.clone())
            .expect("missing API must not permanently disable registration");
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        for (attempt, expected) in [
            "ping",
            "server.ssh_agent.register",
            "ping",
            "server.ssh_agent.register",
        ]
        .into_iter()
        .enumerate()
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        break stream;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "registration did not retry");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(&mut stream).read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], expected);
            if attempt == 1 {
                writeln!(stream, "{}", serde_json::json!({"id": request["id"], "error": {
                    "code": "ssh_agent_unavailable", "message": "replacement server owns the address"
                }})).unwrap();
                continue;
            }
            let result = if expected == "ping" {
                serde_json::json!({"type": "pong", "version": "test", "protocol": crate::protocol::PROTOCOL_VERSION,
                    "capabilities": {"live_handoff": false, "ssh_agent_registration": true}})
            } else {
                serde_json::json!({"type": "ok"})
            };
            writeln!(
                stream,
                "{}",
                serde_json::json!({"id": request["id"], "result": result})
            )
            .unwrap();
            if expected == "server.ssh_agent.register" {
                drop(registration);
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
                break;
            }
        }
        std::fs::remove_file(socket_path).unwrap();
    }
}
