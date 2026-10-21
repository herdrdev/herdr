use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;

use crate::api::peer_channel::PeerChannel;
use crate::api::schema::{
    ErrorResponse, Method, PingParams, Request, ResponseResult, SuccessResponse,
};
use crate::api::{ApiStream, ApiStreamRead};

/// Poll granularity for the bounded federation response reader: how long it
/// sleeps between non-blocking read attempts while waiting for more bytes. Short
/// enough that the wall-clock deadline and a `running` shutdown are observed
/// promptly, long enough not to busy-spin.
const BOUNDED_READ_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionTarget {
    LocalSession(Option<String>),
    SocketPath(PathBuf),
    /// A federation TCP peer. `token`, when set, is sent as the
    /// `federation.hello` line right after connecting, before any request.
    Tcp {
        addr: SocketAddr,
        token: Option<String>,
    },
}

impl ConnectionTarget {
    /// Local socket path for the socket-backed targets. TCP targets have no
    /// socket path and never reach the local-connect path.
    fn socket_path(&self) -> PathBuf {
        match self {
            Self::LocalSession(None) => crate::api::socket_path(),
            Self::LocalSession(Some(session)) => crate::session::api_socket_path_for(Some(session)),
            Self::SocketPath(path) => path.clone(),
            Self::Tcp { .. } => PathBuf::new(),
        }
    }
}

/// Reusable client for Herdr's newline-delimited JSON API.
#[derive(Debug, Clone)]
pub struct ApiClient {
    target: ConnectionTarget,
    /// Carries one-shot bounded requests for an SSH-bridged peer route.
    peer_channel: Option<Arc<PeerChannel>>,
}

impl ApiClient {
    pub fn local() -> Self {
        Self::for_target(ConnectionTarget::LocalSession(None))
    }

    pub fn for_target(target: ConnectionTarget) -> Self {
        Self {
            target,
            peer_channel: None,
        }
    }

    pub(crate) fn with_peer_channel(mut self, peer_channel: Option<Arc<PeerChannel>>) -> Self {
        self.peer_channel = peer_channel;
        self
    }

    pub fn socket_path(&self) -> PathBuf {
        self.target.socket_path()
    }

    pub fn request(&self, request: Request) -> Result<SuccessResponse, ApiClientError> {
        let value = self.request_value(&request)?;
        parse_response_value(value)
    }

    pub fn request_value(&self, request: &Request) -> Result<serde_json::Value, ApiClientError> {
        // request_value is "the first line of request_stream": one request in,
        // the first NDJSON reply out.
        let mut lines = self.request_stream(request)?;
        match lines.next() {
            Some(Ok(value)) => Ok(value),
            Some(Err(err)) => Err(ApiClientError::Io(err)),
            None => Err(ApiClientError::EmptyResponse),
        }
    }

    /// Send `request` and yield every NDJSON reply line until the peer closes
    /// the connection. A round-trip request yields exactly one line; a streaming
    /// request (`events.subscribe`, `pane.stream`, …) yields many and terminates
    /// when the stream closes.
    ///
    /// Local/TCP/socket targets connect then write the request. Direct legacy
    /// `Ssh` targets spawn a one-request bridge; production SSH federation uses
    /// the manager-owned `SocketPath` path instead.
    pub fn request_stream(&self, request: &Request) -> io::Result<ResponseLines> {
        Ok(ResponseLines::over(self.connect_and_write(request)?))
    }

    /// Send `request` and return a [`FederatedStream`] while preserving the
    /// not-delivered versus delivery-unknown boundary used by federation proxy
    /// errors.
    ///
    /// This is the long-lived-stream sibling of [`Self::request_stream`]. It
    /// hands back a reader that enforces a per-frame byte cap, a per-read idle
    /// timeout, and prompt `running`-driven abort. The federated `pane.stream`
    /// proxy reads a peer's live terminal firehose through this, so a malicious
    /// or faulty peer can neither OOM nor indefinitely hang the home connection
    /// thread. It deliberately does not wrap the stream in a [`BufReader`], so
    /// no bytes are hidden between the first-line read and frame piping.
    pub fn open_frame_stream_classified(
        &self,
        request: &Request,
    ) -> Result<FederatedStream, ProxyError> {
        let mut stream = self.connect().map_err(ProxyError::Connect)?;
        write_request_line(&mut stream, request).map_err(ProxyError::Read)?;
        Ok(FederatedStream::over(stream))
    }

    /// Shared connect-and-write for the streaming request paths.
    fn connect_and_write(&self, request: &Request) -> io::Result<ApiStream> {
        let mut stream = self.connect()?;
        write_request_line(&mut stream, request)?;
        Ok(stream)
    }

    pub fn request_value_with_timeout(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<serde_json::Value, ApiClientError> {
        let mut stream = self.connect_and_write(request)?;
        set_timeout_best_effort(&mut stream, TimeoutKind::Send, timeout)?;
        set_timeout_best_effort(&mut stream, TimeoutKind::Recv, timeout)?;
        let mut reader = BufReader::new(stream);
        read_json_line(&mut reader)
    }

    /// Send `request` and read the first NDJSON reply line, bounded by BOTH a
    /// byte cap (`max_bytes`) and a wall-clock `total_timeout` deadline, aborting
    /// early if `running` clears. A peer route's client sends it over the
    /// route's persistent peer channel when that channel is usable.
    ///
    /// Unlike [`Self::request_value_with_timeout`] — whose `SO_RCVTIMEO` only
    /// bounds each individual read, this method bounds the whole response read.
    pub fn request_value_bounded(
        &self,
        request: &Request,
        max_bytes: usize,
        total_timeout: Duration,
        running: Option<&Arc<AtomicBool>>,
    ) -> Result<serde_json::Value, ApiClientError> {
        if let Some(reply) = self.peer_channel.as_ref().and_then(|channel| {
            channel.request(request, max_bytes, total_timeout, running.map(Arc::as_ref))
        }) {
            let line =
                reply.map_err(|(ProxyError::Connect(error) | ProxyError::Read(error))| {
                    ApiClientError::Io(error)
                })?;
            return serde_json::from_str(&line).map_err(ApiClientError::Json);
        }
        let mut stream = self.connect_and_write(request)?;
        let deadline = Instant::now() + total_timeout;
        let line = read_bounded_response_line(&mut stream, max_bytes, deadline, running)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(ApiClientError::EmptyResponse);
        }
        serde_json::from_str(trimmed).map_err(ApiClientError::Json)
    }

    /// Establish the connection and write one federation request while
    /// preserving the not-delivered versus delivery-unknown boundary.
    pub(crate) fn open_proxy_request(&self, request: &Request) -> Result<ApiStream, ProxyError> {
        let mut stream = self.connect().map_err(ProxyError::Connect)?;
        write_request_line(&mut stream, request).map_err(ProxyError::Read)?;
        Ok(stream)
    }

    pub(crate) fn read_proxy_response_bounded(
        mut stream: ApiStream,
        max_bytes: usize,
        total_timeout: Duration,
        running: Option<&Arc<AtomicBool>>,
    ) -> Result<String, ProxyError> {
        let deadline = Instant::now() + total_timeout;
        let line = read_bounded_response_line(&mut stream, max_bytes, deadline, running)
            .map_err(ProxyError::Read)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(ProxyError::Read(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "federation peer returned an empty response",
            )));
        }
        Ok(trimmed.to_string())
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
                write_request_line(&mut stream, &request)?;
                stream.set_polling(true)?;
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

    /// Connect for the socket-backed and TCP transports. For TCP with a token,
    /// the `federation.hello` line is written before this returns.
    fn connect(&self) -> io::Result<ApiStream> {
        match &self.target {
            ConnectionTarget::LocalSession(_) | ConnectionTarget::SocketPath(_) => Ok(
                ApiStream::Local(crate::ipc::connect_local_stream(&self.socket_path())?),
            ),
            ConnectionTarget::Tcp { addr, token } => {
                let mut stream = ApiStream::Tcp(TcpStream::connect(addr)?);
                if let Some(token) = token {
                    write_federation_hello(&mut stream, token)?;
                }
                Ok(stream)
            }
        }
    }
}

/// Iterator over the NDJSON reply lines of a request. Owns the underlying
/// transport, so dropping it tears the connection down.
pub struct ResponseLines {
    reader: BufReader<ApiStream>,
}

impl ResponseLines {
    fn over(stream: ApiStream) -> Self {
        Self {
            reader: BufReader::new(stream),
        }
    }
}

impl Iterator for ResponseLines {
    type Item = io::Result<serde_json::Value>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    return Some(
                        serde_json::from_str(trimmed)
                            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
                    );
                }
                Err(err) => return Some(Err(err)),
            }
        }
    }
}

/// Default per-read idle timeout for a proxied federated `pane.stream`: if the
/// peer produces no complete frame within this window the stream is treated as
/// dead and closed. Deliberately GREATER than the peer's 20s `pane.stream`
/// `PING_INTERVAL` so a healthy-but-quiet stream (heartbeat `ping` frames only)
/// is never killed, while a hung or dead peer is detected and its connection
/// (and the peer-side stream) torn down.
pub const FEDERATION_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(40);

/// A bounded, per-frame-capped streaming NDJSON reader over an [`ApiStream`].
///
/// Built by [`ApiClient::open_frame_stream`] for the federated `pane.stream`
/// proxy. It owns the underlying transport (including any SSH child process), so
/// dropping it tears the peer connection down — the invariant that keeps a
/// proxied stream from leaking on the peer once the home stops reading.
///
/// Unlike [`ResponseLines`], every read is bounded: [`Self::next_frame`] caps a
/// single frame at `max_frame_bytes` (rejecting an over-cap frame before it grows
/// without limit), bounds a quiet peer with a per-read idle timeout, and aborts
/// promptly when `running` clears. Bytes read past a frame's terminating newline
/// are retained in a persistent buffer across calls, so a chunk that carries the
/// tail of one frame and the head of the next never loses the second frame.
pub struct FederatedStream {
    stream: ApiStream,
    /// Bytes read but not yet returned: the in-progress (unterminated) frame, plus
    /// any bytes of the following frame that arrived in the same chunk. Retained
    /// across [`Self::next_frame`] calls so no frame boundary is lost at a chunk
    /// split.
    buf: Vec<u8>,
}

impl FederatedStream {
    fn over(stream: ApiStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    /// Read the next newline-delimited frame, or `Ok(None)` when the peer closed
    /// the stream (a `pane.stream` peer sends `exited` then closes) with no complete
    /// frame left buffered.
    ///
    /// Bounds, each of which surfaces as an `Err` so the caller closes the proxied
    /// stream:
    /// - `max_frame_bytes`: a single frame that exceeds the cap (with or without a
    ///   terminating newline) is rejected before it can drive unbounded allocation.
    /// - `idle_timeout`: recomputed at the start of each call, so it bounds the gap
    ///   between frames, not the whole stream — a peer that goes silent for longer
    ///   than the window is treated as dead.
    /// - `running`: cleared (shutdown) mid-read aborts promptly with `Interrupted`.
    ///
    /// The returned string is the frame's bytes WITHOUT the trailing newline, so
    /// the caller can re-emit it verbatim with a single trailing newline.
    pub fn next_frame(
        &mut self,
        max_frame_bytes: usize,
        idle_timeout: Duration,
        running: &Arc<AtomicBool>,
    ) -> io::Result<Option<String>> {
        // Fast path: a whole frame is already buffered from an earlier chunk. No
        // read (and so no idle-timeout reset) is needed to return it.
        if let Some(frame) = self.take_buffered_frame(max_frame_bytes)? {
            return Ok(Some(frame));
        }

        self.stream.set_polling(true)?;
        let deadline = Instant::now() + idle_timeout;
        let mut chunk = [0u8; 4096];

        loop {
            if !running.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "federated stream read interrupted by shutdown",
                ));
            }

            match self.stream.poll_read(&mut chunk)? {
                ApiStreamRead::Closed => {
                    // Peer/pane ended. A trailing partial frame (no newline) is
                    // discarded — there is no more data to complete it.
                    return Ok(None);
                }
                ApiStreamRead::Data(read) => {
                    self.buf.extend_from_slice(&chunk[..read]);
                    if let Some(frame) = self.take_buffered_frame(max_frame_bytes)? {
                        return Ok(Some(frame));
                    }
                    // No newline yet: bound the in-progress frame so a peer that
                    // never terminates a line cannot grow the buffer without limit.
                    if self.buf.len() > max_frame_bytes {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "federated stream frame is too large",
                        ));
                    }
                }
                ApiStreamRead::Pending => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "federated stream idle timeout: peer sent no frame",
                        ));
                    }
                    std::thread::sleep(BOUNDED_READ_POLL_INTERVAL);
                }
            }
        }
    }

    /// If the retained buffer already holds a complete `\n`-terminated frame, split
    /// it off — returning its bytes minus the newline and keeping the remainder for
    /// the next call — enforcing the per-frame byte cap on the completed frame.
    fn take_buffered_frame(&mut self, max_frame_bytes: usize) -> io::Result<Option<String>> {
        let Some(idx) = self.buf.iter().position(|&b| b == b'\n') else {
            return Ok(None);
        };
        // Cap the whole frame (newline included), matching the one-shot reader.
        if idx + 1 > max_frame_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "federated stream frame is too large",
            ));
        }
        let remainder = self.buf.split_off(idx + 1);
        self.buf.truncate(idx); // drop the trailing newline
        let frame_bytes = std::mem::replace(&mut self.buf, remainder);
        String::from_utf8(frame_bytes)
            .map(Some)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }
}

enum TimeoutKind {
    Send,
    Recv,
}

fn set_timeout_best_effort(
    stream: &mut ApiStream,
    kind: TimeoutKind,
    timeout: Duration,
) -> io::Result<()> {
    let result = match kind {
        TimeoutKind::Send => stream.set_send_timeout(Some(timeout)),
        TimeoutKind::Recv => stream.set_recv_timeout(Some(timeout)),
    };
    match result {
        Ok(()) => Ok(()),
        // Named-pipe / some transports report timeouts as unsupported; the
        // request still proceeds without an enforced deadline.
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

/// Which delivery phase of a federation proxy failed.
///
/// The distinction is the home daemon's delivered-vs-unknown verdict:
/// [`ProxyError::Connect`] happens before a transport is established, so the
/// request was not delivered. [`ProxyError::Read`] happens after establishment,
/// including while writing the request, so the peer may have received all or
/// part of it and callers must not retry blindly.
#[derive(Debug)]
pub enum ProxyError {
    /// Transport establishment or request serialization failed; not delivered.
    Connect(io::Error),
    /// Request write or response read failed after connection; delivery unknown.
    Read(io::Error),
}
impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "{err}"),
            Self::Read(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ProxyError {}

fn write_request_line(stream: &mut ApiStream, request: &Request) -> io::Result<()> {
    let encoded = serde_json::to_string(request)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    stream.write_all(encoded.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

struct DeadlineReader<'a> {
    stream: &'a mut ApiStream,
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
            match self.stream.poll_read(buffer)? {
                ApiStreamRead::Data(count) => return Ok(count),
                ApiStreamRead::Closed => return Ok(0),
                ApiStreamRead::Pending => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }
}

/// Write the versioned `federation.hello` line. Must match the exact shape the
/// listener expects — both sides share [`crate::api::federation::FederationHello`].
///
/// `FederationHello::new` stamps the current `FEDERATION_PROTOCOL_VERSION`; this
/// then stamps the persisted per-install machine id
/// ([`crate::persist::machine::get_or_create`]) so a receiving peer that pins an
/// `expected_node_id` can verify it. The token remains the authenticator.
fn write_federation_hello(stream: &mut ApiStream, token: &str) -> io::Result<()> {
    let line = crate::api::federation::FederationHello::new(token)
        .with_machine_id(crate::persist::machine::get_or_create())
        .to_line()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn read_json_line<T, R>(reader: &mut R) -> Result<T, ApiClientError>
where
    T: DeserializeOwned,
    R: BufRead,
{
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 || line.trim().is_empty() {
        return Err(ApiClientError::EmptyResponse);
    }
    serde_json::from_str(&line).map_err(ApiClientError::Json)
}

/// Read one newline-terminated response line off `stream`, bounded by BOTH a
/// wall-clock `deadline` (re-checked between reads) and a `max_bytes` cap on the
/// accumulated line, and responsive to `running` clearing.
///
/// The stream is put in non-blocking (`set_polling`) mode and drained one chunk
/// at a time, so no single read can block past the deadline: a peer that trickles
/// bytes, stalls without ever sending a newline, or returns an oversized line is
/// bounded by `deadline`/`max_bytes` rather than allowed to run (and allocate)
/// without limit. Any of the bounds, an early EOF, or a cleared `running` flag
/// surfaces as an `Err` so the caller degrades that peer. Only the bytes up to
/// (and including) the first newline are retained; anything a peer piles on after
/// it is discarded, so trailing garbage cannot grow the buffer either.
fn read_bounded_response_line(
    stream: &mut ApiStream,
    max_bytes: usize,
    deadline: Instant,
    running: Option<&Arc<AtomicBool>>,
) -> io::Result<String> {
    stream.set_polling(true)?;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];

    let result = loop {
        if running.is_some_and(|flag| !flag.load(Ordering::Relaxed)) {
            break Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "federation poll response read interrupted by shutdown",
            ));
        }

        match stream.poll_read(&mut chunk) {
            Ok(ApiStreamRead::Closed) => {
                break Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "federation peer closed the connection before a full response line",
                ));
            }
            Ok(ApiStreamRead::Data(read)) => {
                let data = &chunk[..read];
                let newline = data.iter().position(|&b| b == b'\n');
                let take = newline.map(|i| i + 1).unwrap_or(read);
                buf.extend_from_slice(&data[..take]);
                if buf.len() > max_bytes {
                    break Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "federation response line is too large",
                    ));
                }
                if newline.is_some() {
                    break String::from_utf8(buf)
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
                }
            }
            Ok(ApiStreamRead::Pending) => {
                if Instant::now() >= deadline {
                    break Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out reading federation response",
                    ));
                }
                std::thread::sleep(BOUNDED_READ_POLL_INTERVAL);
            }
            Err(err) => break Err(err),
        }
    };

    // Best-effort restore of blocking mode; the connection is torn down on error
    // anyway, and the caller drops the stream after a successful line.
    let _ = stream.set_polling(false);
    result
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

/// Failure parsing a federation peer `endpoint` string into a [`ConnectionTarget`].
#[derive(Debug)]
pub enum EndpointParseError {
    /// The scheme is neither `tcp://` nor `ssh://`.
    UnknownScheme(String),
    /// The authority (host, or host:port for TCP) was empty.
    MissingHost,
    /// A `tcp://host:port` authority did not resolve to a socket address.
    UnresolvedTcpAddress {
        authority: String,
        source: io::Error,
    },
}

impl fmt::Display for EndpointParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownScheme(endpoint) => write!(
                f,
                "unsupported federation endpoint scheme in {endpoint:?}; expected tcp:// or ssh://"
            ),
            Self::MissingHost => write!(f, "federation endpoint is missing a host"),
            Self::UnresolvedTcpAddress { authority, source } => {
                write!(
                    f,
                    "could not resolve tcp federation endpoint {authority:?}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for EndpointParseError {}

/// Resolve a federation peer endpoint.
///
/// TCP endpoints become immediately connectable targets. SSH endpoints remain
/// destinations until the manager resolves them into a shared saved-machine
/// `remote-api-bridge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FederationEndpoint {
    Target(ConnectionTarget),
    Ssh { destination: String },
}

pub fn endpoint_to_target(
    endpoint: &str,
    token: Option<String>,
) -> Result<FederationEndpoint, EndpointParseError> {
    if let Some(authority) = endpoint.strip_prefix("tcp://") {
        if authority.is_empty() {
            return Err(EndpointParseError::MissingHost);
        }
        let addr = authority
            .to_socket_addrs()
            .map_err(|source| EndpointParseError::UnresolvedTcpAddress {
                authority: authority.to_string(),
                source,
            })?
            .next()
            .ok_or_else(|| EndpointParseError::UnresolvedTcpAddress {
                authority: authority.to_string(),
                source: io::Error::new(io::ErrorKind::NotFound, "no addresses resolved"),
            })?;
        Ok(FederationEndpoint::Target(ConnectionTarget::Tcp {
            addr,
            token,
        }))
    } else if let Some(authority) = endpoint.strip_prefix("ssh://") {
        let host = authority
            .split_once('@')
            .map_or(authority, |(_, host)| host);
        if host.is_empty() {
            return Err(EndpointParseError::MissingHost);
        }
        Ok(FederationEndpoint::Ssh {
            destination: authority.to_string(),
        })
    } else {
        Err(EndpointParseError::UnknownScheme(endpoint.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A loopback TCP peer that runs `serve` on the accepted socket, plus the
    /// address to connect to. Joining teardown-safe.
    fn loopback_peer(
        serve: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback peer");
        let addr = listener.local_addr().expect("peer addr");
        let handle = std::thread::spawn(move || {
            let (sock, _) = listener.accept().expect("accept poll client");
            serve(sock);
        });
        (addr, handle)
    }

    /// A peer returning a line larger than the cap (no newline in sight) makes the
    /// bounded reader error at ~the cap instead of accumulating without limit.
    #[test]
    fn bounded_read_rejects_an_over_cap_line() {
        let (addr, peer) = loopback_peer(|mut sock| {
            // Far more than the 32-byte cap used below, and no newline.
            let _ = sock.write_all(&vec![b'x'; 4096]);
            std::thread::sleep(Duration::from_millis(200));
        });

        let mut stream = ApiStream::Tcp(TcpStream::connect(addr).expect("connect peer"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let err = read_bounded_response_line(&mut stream, 32, deadline, None).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "federation response line is too large");
        peer.join().unwrap();
    }

    /// A peer that trickles bytes but never terminates a line is bounded by the
    /// wall-clock deadline, not left to read forever.
    #[test]
    fn bounded_read_honors_the_deadline_when_a_peer_stalls() {
        let (addr, peer) = loopback_peer(|mut sock| {
            let _ = sock.write_all(b"partial-with-no-newline");
            std::thread::sleep(Duration::from_millis(400));
        });

        let mut stream = ApiStream::Tcp(TcpStream::connect(addr).expect("connect peer"));
        let deadline = Instant::now() + Duration::from_millis(150);
        let started = Instant::now();
        let err = read_bounded_response_line(&mut stream, 64 * 1024, deadline, None).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the read did not stop near the deadline"
        );
        peer.join().unwrap();
    }

    /// Clearing `running` mid-read aborts it promptly, so shutdown (which joins
    /// the poll threads) does not hang on a silent peer.
    #[test]
    fn bounded_read_aborts_when_running_clears() {
        let (addr, peer) = loopback_peer(|sock| {
            // Silent but open: only the running flag can end the read.
            std::thread::sleep(Duration::from_millis(500));
            drop(sock);
        });

        let mut stream = ApiStream::Tcp(TcpStream::connect(addr).expect("connect peer"));
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag.store(false, Ordering::Relaxed);
        });

        // Far-future deadline: only the cleared running flag can end this read.
        let deadline = Instant::now() + Duration::from_secs(30);
        let started = Instant::now();
        let err = read_bounded_response_line(&mut stream, 64 * 1024, deadline, Some(&running))
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the read did not abort promptly on shutdown"
        );
        peer.join().unwrap();
    }

    /// Two whole frames delivered in a SINGLE chunk both surface, in order: the
    /// first `next_frame` returns frame one and RETAINS frame two's bytes, which
    /// the second call returns from the buffer without another read. Then the peer
    /// close surfaces as `Ok(None)`.
    #[test]
    fn next_frame_surfaces_two_frames_from_one_chunk() {
        let (addr, peer) = loopback_peer(|mut sock| {
            // Both frames in one write (one chunk over loopback), then close.
            let _ = sock.write_all(b"{\"frame\":\"one\"}\n{\"frame\":\"two\"}\n");
            let _ = sock.flush();
            // Hold briefly so the reader observes the bytes before EOF, then close.
            std::thread::sleep(Duration::from_millis(50));
        });

        let running = Arc::new(AtomicBool::new(true));
        let mut stream = FederatedStream::over(ApiStream::Tcp(
            TcpStream::connect(addr).expect("connect peer"),
        ));
        let idle = Duration::from_secs(5);

        let first = stream
            .next_frame(64 * 1024, idle, &running)
            .expect("first frame io-ok")
            .expect("first frame present");
        assert_eq!(first, "{\"frame\":\"one\"}");

        let second = stream
            .next_frame(64 * 1024, idle, &running)
            .expect("second frame io-ok")
            .expect("second frame present");
        assert_eq!(second, "{\"frame\":\"two\"}");

        // Peer closed after both frames → end of stream.
        assert!(
            stream
                .next_frame(64 * 1024, idle, &running)
                .expect("third read io-ok")
                .is_none(),
            "stream did not end when the peer closed"
        );
        peer.join().unwrap();
    }

    /// A single frame larger than the per-frame cap (no newline in sight) makes
    /// `next_frame` error at ~the cap instead of buffering without limit.
    #[test]
    fn next_frame_rejects_an_over_cap_frame() {
        let (addr, peer) = loopback_peer(|mut sock| {
            let _ = sock.write_all(&vec![b'x'; 4096]); // >32-byte cap, no newline
            std::thread::sleep(Duration::from_millis(200));
        });

        let running = Arc::new(AtomicBool::new(true));
        let mut stream = FederatedStream::over(ApiStream::Tcp(
            TcpStream::connect(addr).expect("connect peer"),
        ));
        let err = stream
            .next_frame(32, Duration::from_secs(5), &running)
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "federated stream frame is too large");
        peer.join().unwrap();
    }

    /// A newline-TERMINATED frame larger than the cap is rejected too — the
    /// completed-frame cap path (`take_buffered_frame`), distinct from the
    /// unterminated-growth path above. A peer that sends one complete but
    /// oversized line cannot slip it through, and it never reaches the client.
    #[test]
    fn next_frame_rejects_an_over_cap_terminated_frame() {
        let (addr, peer) = loopback_peer(|mut sock| {
            // 40 bytes + '\n' in a single write: a COMPLETE line of 40 > the
            // 32-byte cap. The newline arrives in the same chunk, so
            // `take_buffered_frame`'s completed-frame cap fires before the
            // unterminated-growth check is ever consulted.
            let mut line = vec![b'x'; 40];
            line.push(b'\n');
            let _ = sock.write_all(&line);
            std::thread::sleep(Duration::from_millis(200));
        });

        let running = Arc::new(AtomicBool::new(true));
        let mut stream = FederatedStream::over(ApiStream::Tcp(
            TcpStream::connect(addr).expect("connect peer"),
        ));
        let err = stream
            .next_frame(32, Duration::from_secs(5), &running)
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "federated stream frame is too large");
        peer.join().unwrap();
    }

    /// Clearing `running` mid-read aborts `next_frame` promptly, so a proxied
    /// stream does not hang shutdown on a silent peer.
    #[test]
    fn next_frame_aborts_when_running_clears() {
        let (addr, peer) = loopback_peer(|sock| {
            std::thread::sleep(Duration::from_millis(500)); // silent but open
            drop(sock);
        });

        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag.store(false, Ordering::Relaxed);
        });

        let mut stream = FederatedStream::over(ApiStream::Tcp(
            TcpStream::connect(addr).expect("connect peer"),
        ));
        let started = Instant::now();
        // Far-future idle timeout: only the cleared running flag can end this read.
        let err = stream
            .next_frame(64 * 1024, Duration::from_secs(30), &running)
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the read did not abort promptly on shutdown"
        );
        peer.join().unwrap();
    }

    /// The idle timeout fires only PAST the threshold: a frame that arrives inside
    /// the window is delivered, and a peer that then goes silent past the window
    /// times out (not before it).
    #[test]
    fn next_frame_idle_timeout_fires_only_past_the_threshold() {
        let (addr, peer) = loopback_peer(|mut sock| {
            // A frame well inside the window, then silence.
            std::thread::sleep(Duration::from_millis(40));
            let _ = sock.write_all(b"{\"frame\":\"tick\"}\n");
            let _ = sock.flush();
            std::thread::sleep(Duration::from_millis(600)); // then go quiet
        });

        let running = Arc::new(AtomicBool::new(true));
        let mut stream = FederatedStream::over(ApiStream::Tcp(
            TcpStream::connect(addr).expect("connect peer"),
        ));
        let idle = Duration::from_millis(200);

        // Frame inside the window is delivered (idle timeout does NOT fire early).
        let frame = stream
            .next_frame(64 * 1024, idle, &running)
            .expect("frame io-ok")
            .expect("frame present");
        assert_eq!(frame, "{\"frame\":\"tick\"}");

        // Peer now silent: the next read times out, and only after the threshold.
        let started = Instant::now();
        let err = stream.next_frame(64 * 1024, idle, &running).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() >= idle,
            "the idle timeout fired before its threshold"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the idle timeout did not fire near its threshold"
        );
        peer.join().unwrap();
    }

    #[test]
    fn local_session_target_resolves_named_session_socket() {
        let client = ApiClient::for_target(ConnectionTarget::LocalSession(Some("work".into())));
        assert!(client.socket_path().ends_with("sessions/work/herdr.sock"));
    }

    #[test]
    fn status_timeout_closes_a_stalled_probe() {
        #[cfg(unix)]
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

    #[test]
    fn tcp_target_has_no_socket_path() {
        let tcp = ApiClient::for_target(ConnectionTarget::Tcp {
            addr: "127.0.0.1:9000".parse().unwrap(),
            token: Some("t".into()),
        });
        assert_eq!(tcp.socket_path(), PathBuf::new());
    }

    #[test]
    fn endpoint_tcp_resolves_to_a_tcp_target_with_token() {
        let target = endpoint_to_target("tcp://127.0.0.1:9000", Some("s3cret".into()))
            .expect("tcp endpoint parses");
        match target {
            FederationEndpoint::Target(ConnectionTarget::Tcp { addr, token }) => {
                assert_eq!(addr, "127.0.0.1:9000".parse::<SocketAddr>().unwrap());
                assert_eq!(token.as_deref(), Some("s3cret"));
            }
            other => panic!("expected tcp target, got {other:?}"),
        }
    }

    #[test]
    fn endpoint_ssh_parses_user_and_host_and_ignores_token() {
        let target = endpoint_to_target("ssh://alice@host.example", Some("ignored".into()))
            .expect("ssh endpoint parses");
        assert_eq!(
            target,
            FederationEndpoint::Ssh {
                destination: "alice@host.example".into(),
            }
        );
    }

    #[test]
    fn endpoint_ssh_without_user_parses_bare_host() {
        let target =
            endpoint_to_target("ssh://host.example", None).expect("bare ssh endpoint parses");
        assert_eq!(
            target,
            FederationEndpoint::Ssh {
                destination: "host.example".into(),
            }
        );
    }

    #[test]
    fn endpoint_rejects_unknown_scheme_and_empty_host() {
        assert!(matches!(
            endpoint_to_target("http://host:80", None),
            Err(EndpointParseError::UnknownScheme(_))
        ));
        assert!(matches!(
            endpoint_to_target("garbage", None),
            Err(EndpointParseError::UnknownScheme(_))
        ));
        assert!(matches!(
            endpoint_to_target("ssh://", None),
            Err(EndpointParseError::MissingHost)
        ));
        assert!(matches!(
            endpoint_to_target("tcp://", None),
            Err(EndpointParseError::MissingHost)
        ));
    }

    #[test]
    fn endpoint_tcp_missing_port_is_rejected() {
        assert!(matches!(
            endpoint_to_target("tcp://127.0.0.1", None),
            Err(EndpointParseError::UnresolvedTcpAddress { .. })
        ));
    }
}
