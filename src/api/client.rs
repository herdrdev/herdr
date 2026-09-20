use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;
use serde::de::DeserializeOwned;

use crate::api::schema::{
    ErrorResponse, Method, PingParams, Request, ResponseResult, SuccessResponse,
};
use crate::ipc::LocalStream;

/// API connection target resolved by clients at the process edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionTarget {
    LocalSession(Option<String>),
    SocketPath(PathBuf),
}

impl ConnectionTarget {
    fn socket_path(&self) -> PathBuf {
        match self {
            Self::LocalSession(None) => crate::api::socket_path(),
            Self::LocalSession(Some(name)) => crate::session::api_socket_path_for(Some(name)),
            Self::SocketPath(path) => path.clone(),
        }
    }
}

/// Reusable client for Herdr's newline-delimited JSON API.
#[derive(Debug, Clone)]
pub struct ApiClient {
    target: ConnectionTarget,
}

impl ApiClient {
    pub fn local() -> Self {
        Self::for_target(ConnectionTarget::LocalSession(None))
    }

    pub fn for_target(target: ConnectionTarget) -> Self {
        Self { target }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.target.socket_path()
    }

    pub fn request(&self, request: Request) -> Result<SuccessResponse, ApiClientError> {
        let value = self.request_value(&request)?;
        parse_response_value(value)
    }

    pub fn request_value(&self, request: &Request) -> Result<serde_json::Value, ApiClientError> {
        let mut stream = self.connect()?;
        write_request(&mut stream, request)?;

        let mut reader = BufReader::new(stream);
        read_json_line(&mut reader)
    }

    pub fn request_value_with_timeout(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<serde_json::Value, ApiClientError> {
        let mut stream = self.connect()?;
        set_timeout_best_effort(&stream, TimeoutKind::Send, timeout)?;
        set_timeout_best_effort(&stream, TimeoutKind::Recv, timeout)?;
        write_request(&mut stream, request)?;

        let mut reader = BufReader::new(stream);
        read_json_line(&mut reader)
    }

    pub fn status(&self) -> Result<crate::api::RuntimeStatus, ApiClientError> {
        self.read_status(None)
    }

    pub(crate) fn status_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<crate::api::RuntimeStatus, ApiClientError> {
        self.read_status(Some(timeout))
    }

    fn read_status(
        &self,
        timeout: Option<Duration>,
    ) -> Result<crate::api::RuntimeStatus, ApiClientError> {
        let request = Request {
            id: "api-client:status".into(),
            method: Method::Ping(PingParams::default()),
        };
        let response = match timeout {
            Some(timeout) => {
                let mut stream = self.connect()?;
                write_request(&mut stream, &request)?;
                crate::ipc::set_local_stream_polling(&mut stream, true)?;
                let mut reader = BufReader::new(DeadlineReader {
                    stream: &mut stream,
                    deadline: Instant::now() + timeout,
                });
                parse_response_value(read_json_line(&mut reader)?)?
            }
            None => self.request(request)?,
        };
        match response.result {
            ResponseResult::Pong {
                version,
                protocol,
                capabilities,
            } => Ok(crate::api::RuntimeStatus {
                version: Some(version),
                protocol: Some(protocol),
                capabilities,
            }),
            result => Err(ApiClientError::UnexpectedResult(format!("{result:?}"))),
        }
    }

    fn connect(&self) -> io::Result<LocalStream> {
        crate::ipc::connect_local_stream(&self.socket_path())
    }
}

enum TimeoutKind {
    Send,
    Recv,
}

fn set_timeout_best_effort(
    stream: &LocalStream,
    kind: TimeoutKind,
    timeout: Duration,
) -> io::Result<()> {
    let result = match kind {
        TimeoutKind::Send => stream.set_send_timeout(Some(timeout)),
        TimeoutKind::Recv => stream.set_recv_timeout(Some(timeout)),
    };
    match result {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(err) if err.kind() == io::ErrorKind::Unsupported => Ok(()),
        Err(err) => Err(err),
    }
}

#[derive(Debug)]
pub enum ApiClientError {
    Io(io::Error),
    Json(serde_json::Error),
    ErrorResponse(ErrorResponse),
    EmptyResponse,
    UnexpectedResult(String),
}

impl fmt::Display for ApiClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Json(err) => write!(f, "{err}"),
            Self::ErrorResponse(response) => write!(f, "{}", response.error.message),
            Self::EmptyResponse => write!(f, "empty api response"),
            Self::UnexpectedResult(result) => write!(f, "unexpected api result: {result}"),
        }
    }
}

impl std::error::Error for ApiClientError {}

impl From<io::Error> for ApiClientError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for ApiClientError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

fn write_request(stream: &mut LocalStream, request: &Request) -> Result<(), ApiClientError> {
    stream.write_all(serde_json::to_string(request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

struct DeadlineReader<'a> {
    stream: &'a mut LocalStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "server status probe timed out",
                ));
            }
            // Windows named pipes have no read timeout; peek-before-read keeps
            // both idle and partial responses subject to the same deadline.
            match crate::ipc::poll_local_stream_read_count(self.stream, buffer)? {
                crate::ipc::LocalStreamReadCount::Data(count) => return Ok(count),
                crate::ipc::LocalStreamReadCount::Closed => return Ok(0),
                crate::ipc::LocalStreamReadCount::Pending => {
                    std::thread::sleep(Duration::from_millis(2))
                }
            }
        }
    }
}

fn read_json_line<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<T, ApiClientError> {
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 || line.trim().is_empty() {
        return Err(ApiClientError::EmptyResponse);
    }
    serde_json::from_str(&line).map_err(ApiClientError::Json)
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum WireResponse {
    Success(Box<SuccessResponse>),
    Error(ErrorResponse),
}

pub(crate) fn parse_response_value(
    value: serde_json::Value,
) -> Result<SuccessResponse, ApiClientError> {
    match serde_json::from_value(value)? {
        WireResponse::Success(response) => Ok(*response),
        WireResponse::Error(response) => Err(ApiClientError::ErrorResponse(response)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_session_target_resolves_named_session_socket() {
        let client = ApiClient::for_target(ConnectionTarget::LocalSession(Some("work".into())));
        assert!(client.socket_path().ends_with("sessions/work/herdr.sock"));
    }

    #[test]
    fn status_timeout_closes_a_stalled_probe() {
        use interprocess::local_socket::traits::Listener as _;
        let path =
            std::env::temp_dir().join(format!("herdr-status-timeout-{}.sock", std::process::id()));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let server = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap()["method"],
                "ping"
            );
            std::thread::sleep(Duration::from_millis(300));
        });
        let client = ApiClient::for_target(ConnectionTarget::SocketPath(path.clone()));
        let error = client
            .status_with_timeout(Duration::from_millis(100))
            .unwrap_err();
        assert!(
            matches!(error, ApiClientError::Io(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
        );
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn socket_path_target_uses_explicit_path() {
        let path = PathBuf::from("/tmp/herdr-test.sock");
        let client = ApiClient::for_target(ConnectionTarget::SocketPath(path.clone()));
        assert_eq!(client.socket_path(), path);
    }
}
