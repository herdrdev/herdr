use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneGraphicsSetParams, PaneGraphicsStreamParams, Request,
    ResponseResult, SuccessResponse,
};
use crate::api::ApiRequestSender;
use crate::ipc::{is_connection_closed_error, LocalStream};

use super::{
    api_response_outcome, dispatch_to_app_with_timeout, write_json_line,
    write_json_line_allow_disconnect, write_text_line_allow_disconnect, APP_RESPONSE_TIMEOUT,
    CONNECTION_POLL_INTERVAL,
};

const MAX_STREAM_FRAME_HEADER_BYTES: usize = 64 * 1024;
const STREAM_FRAME_BODY_CHUNK_BYTES: usize = 64 * 1024;
const STREAM_FRAME_HEADER_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_FRAME_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_FRAME_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_FRAME_BODY_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_FALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(1);
const STREAM_FALLBACK_FAST_POLLS: u8 = 32;
static NEXT_PANE_GRAPHICS_STREAM_OWNER: AtomicU64 = AtomicU64::new(1);
static REGISTERED_STREAM_COUNT: AtomicUsize = AtomicUsize::new(0);
static REGISTERED_STREAMS: OnceLock<Mutex<HashMap<String, Weak<AtomicBool>>>> = OnceLock::new();

#[derive(serde::Deserialize)]
struct FrameHeader {
    format: crate::api::schema::PaneGraphicsFormat,
    image_width: u32,
    image_height: u32,
    data_length: usize,
    #[serde(default)]
    placement: crate::api::schema::PaneGraphicsPlacementParams,
}

#[derive(Clone, Copy)]
struct ReadTimeouts {
    header_idle: Duration,
    header_total: Duration,
    body_idle: Duration,
    body_total: Duration,
}

const READ_TIMEOUTS: ReadTimeouts = ReadTimeouts {
    header_idle: STREAM_FRAME_HEADER_IDLE_TIMEOUT,
    header_total: STREAM_FRAME_HEADER_TIMEOUT,
    body_idle: STREAM_FRAME_BODY_IDLE_TIMEOUT,
    body_total: STREAM_FRAME_BODY_TIMEOUT,
};

pub(super) fn serve(
    stream: LocalStream,
    request_id: String,
    params: PaneGraphicsStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    serve_with_open_timeout(
        stream,
        request_id,
        params,
        api_tx,
        running,
        APP_RESPONSE_TIMEOUT,
    )
}

fn serve_with_open_timeout(
    stream: LocalStream,
    request_id: String,
    params: PaneGraphicsStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    open_timeout: Duration,
) -> std::io::Result<()> {
    serve_with_timeouts(
        stream,
        request_id,
        params,
        api_tx,
        running,
        open_timeout,
        READ_TIMEOUTS,
    )
}

fn serve_with_timeouts(
    mut stream: LocalStream,
    request_id: String,
    mut params: PaneGraphicsStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    open_timeout: Duration,
    read_timeouts: ReadTimeouts,
) -> std::io::Result<()> {
    let pane_id = params.pane_id.clone();
    let owner = next_owner();
    params.owner = owner.clone();
    let open_response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneGraphicsStreamOpen(params),
        },
        api_tx,
        Some(open_timeout),
    );
    if api_response_outcome(&open_response) != "ok" {
        let write_result = write_text_line_allow_disconnect(&mut stream, &open_response);
        clear_layer(&pane_id, &owner, api_tx);
        write_result?;
        return Ok(());
    }

    if let Err(err) = write_json_line(
        &mut stream,
        &SuccessResponse {
            id: request_id.clone(),
            result: ResponseResult::Ok {},
        },
    ) {
        clear_layer(&pane_id, &owner, api_tx);
        if is_connection_closed_error(&err) {
            return Ok(());
        }
        return Err(err);
    }

    let stream_active = Arc::new(AtomicBool::new(true));
    register_stream(&owner, &stream_active);
    let result = serve_frames(
        &mut stream,
        &request_id,
        &owner,
        &pane_id,
        api_tx,
        running,
        &stream_active,
        read_timeouts,
    );
    unregister_stream(&owner);
    clear_layer(&pane_id, &owner, api_tx);
    result
}

fn serve_frames(
    stream: &mut LocalStream,
    request_id: &str,
    owner: &str,
    pane_id: &str,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    stream_active: &Arc<AtomicBool>,
    timeouts: ReadTimeouts,
) -> std::io::Result<()> {
    let mut frame_seq = 0_u64;
    while stream_is_running(running, stream_active) {
        let Some(header_line) = read_line(
            stream,
            running,
            stream_active,
            MAX_STREAM_FRAME_HEADER_BYTES,
            timeouts.header_idle,
            timeouts.header_total,
        )?
        else {
            return Ok(());
        };
        let header_line = header_line.trim();
        if header_line.is_empty() {
            continue;
        }
        let header = match serde_json::from_str::<FrameHeader>(header_line) {
            Ok(header) => header,
            Err(err) => {
                write_json_line_allow_disconnect(
                    stream,
                    &ErrorResponse {
                        id: request_id.to_string(),
                        error: ErrorBody {
                            code: "invalid_frame".into(),
                            message: format!("invalid frame header: {err}"),
                        },
                    },
                )?;
                return Ok(());
            }
        };
        if header.data_length == 0 {
            write_json_line_allow_disconnect(
                stream,
                &ErrorResponse {
                    id: request_id.to_string(),
                    error: ErrorBody {
                        code: "invalid_frame".into(),
                        message: "frame data_length must be greater than zero".into(),
                    },
                },
            )?;
            return Ok(());
        }
        if header.data_length > crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES {
            write_json_line_allow_disconnect(
                stream,
                &ErrorResponse {
                    id: request_id.to_string(),
                    error: ErrorBody {
                        code: "image_too_large".into(),
                        message: "frame data is too large".into(),
                    },
                },
            )?;
            return Ok(());
        }

        let Some(data) = read_exact(
            stream,
            header.data_length,
            running,
            stream_active,
            timeouts.body_idle,
            timeouts.body_total,
        )?
        else {
            return Ok(());
        };

        frame_seq = frame_seq.saturating_add(1);
        let frame_id = format!("{request_id}:frame:{frame_seq}");
        let response = dispatch_to_app_with_timeout(
            Request {
                id: frame_id,
                method: Method::PaneGraphicsStreamSet(PaneGraphicsSetParams {
                    pane_id: pane_id.to_string(),
                    owner: owner.to_string(),
                    format: header.format,
                    image_width: header.image_width,
                    image_height: header.image_height,
                    data: Some(data),
                    data_base64: String::new(),
                    placement: header.placement,
                }),
            },
            api_tx,
            Some(APP_RESPONSE_TIMEOUT),
        );
        if api_response_outcome(&response) != "ok" {
            write_text_line_allow_disconnect(stream, &response)?;
            return Ok(());
        }
    }

    Ok(())
}

fn stream_registry() -> &'static Mutex<HashMap<String, Weak<AtomicBool>>> {
    REGISTERED_STREAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_stream(owner: &str, active: &Arc<AtomicBool>) {
    let Ok(mut streams) = stream_registry().lock() else {
        active.store(false, Ordering::Release);
        return;
    };
    if streams
        .insert(owner.to_string(), Arc::downgrade(active))
        .is_none()
    {
        REGISTERED_STREAM_COUNT.fetch_add(1, Ordering::Release);
    }
}

fn unregister_stream(owner: &str) {
    let Ok(mut streams) = stream_registry().lock() else {
        return;
    };
    if streams.remove(owner).is_some() {
        REGISTERED_STREAM_COUNT.fetch_sub(1, Ordering::Release);
    }
}

pub(crate) fn cancel_inactive_streams(mut is_active: impl FnMut(&str) -> bool) {
    if REGISTERED_STREAM_COUNT.load(Ordering::Acquire) == 0 {
        return;
    }
    let Ok(mut streams) = stream_registry().lock() else {
        return;
    };
    let before = streams.len();
    streams.retain(|owner, active| {
        let keep = is_active(owner);
        if !keep {
            if let Some(active) = active.upgrade() {
                active.store(false, Ordering::Release);
            }
        }
        keep
    });
    REGISTERED_STREAM_COUNT.fetch_sub(before.saturating_sub(streams.len()), Ordering::Release);
}

fn stream_is_running(running: &AtomicBool, stream_active: &AtomicBool) -> bool {
    running.load(Ordering::Relaxed) && stream_active.load(Ordering::Acquire)
}

fn next_owner() -> String {
    let id = NEXT_PANE_GRAPHICS_STREAM_OWNER.fetch_add(1, Ordering::Relaxed);
    format!("pane.graphics.stream:{}:{id}", std::process::id())
}

fn clear_layer(pane_id: &str, owner: &str, api_tx: &ApiRequestSender) {
    let _response = dispatch_to_app_with_timeout(
        Request {
            id: format!("pane.graphics.stream.clear:{pane_id}"),
            method: Method::PaneGraphicsStreamClose(PaneGraphicsStreamParams {
                pane_id: pane_id.to_string(),
                owner: owner.to_string(),
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
}

fn read_line(
    stream: &mut LocalStream,
    running: &Arc<AtomicBool>,
    stream_active: &Arc<AtomicBool>,
    max_bytes: usize,
    idle_timeout: Duration,
    total_timeout: Duration,
) -> std::io::Result<Option<String>> {
    with_timed_reads(stream, |stream, mut wait| {
        let mut bytes = Vec::new();
        let mut byte = [0_u8; 1];
        let mut total_deadline = None;
        let mut idle_deadline = None;

        loop {
            if !stream_is_running(running, stream_active) {
                return Ok(None);
            }
            ensure_before_deadlines(
                idle_deadline,
                total_deadline,
                "timed out reading stream frame header",
            )?;
            match stream.read(&mut byte) {
                Ok(0) => return Ok(None),
                Ok(_) => {
                    wait.on_progress();
                    let now = Instant::now();
                    let total_deadline_at =
                        *total_deadline.get_or_insert_with(|| now + total_timeout);
                    idle_deadline = Some(now + idle_timeout);
                    if now >= total_deadline_at {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out reading stream frame header",
                        ));
                    }
                    bytes.push(byte[0]);
                    if byte[0] == b'\n' {
                        return String::from_utf8(bytes)
                            .map(Some)
                            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
                    }
                    if bytes.len() > max_bytes {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "stream frame header is too large",
                        ));
                    }
                }
                Err(err) if read_should_retry(&err) => {
                    wait.after_retry(idle_deadline, total_deadline);
                }
                Err(err) if is_connection_closed_error(&err) => return Ok(None),
                Err(err) => return Err(err),
            }
        }
    })
}

fn read_exact(
    stream: &mut LocalStream,
    len: usize,
    running: &Arc<AtomicBool>,
    stream_active: &Arc<AtomicBool>,
    idle_timeout: Duration,
    total_timeout: Duration,
) -> std::io::Result<Option<Vec<u8>>> {
    with_timed_reads(stream, |stream, mut wait| {
        let mut data = Vec::new();
        let mut chunk = vec![0_u8; STREAM_FRAME_BODY_CHUNK_BYTES.min(len)];
        let total_deadline = Instant::now() + total_timeout;
        let mut idle_deadline = Instant::now() + idle_timeout;

        while data.len() < len {
            if !stream_is_running(running, stream_active) {
                return Ok(None);
            }
            ensure_before_deadlines(
                Some(idle_deadline),
                Some(total_deadline),
                "timed out reading stream frame body",
            )?;
            let remaining = len - data.len();
            let read_len = remaining.min(chunk.len());
            match stream.read(&mut chunk[..read_len]) {
                Ok(0) if data.is_empty() => return Ok(None),
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "stream ended mid-frame",
                    ));
                }
                Ok(n) => {
                    wait.on_progress();
                    let now = Instant::now();
                    if now >= total_deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out reading stream frame body",
                        ));
                    }
                    data.extend_from_slice(&chunk[..n]);
                    idle_deadline = now + idle_timeout;
                }
                Err(err) if read_should_retry(&err) => {
                    wait.after_retry(Some(idle_deadline), Some(total_deadline));
                }
                Err(err) if is_connection_closed_error(&err) && data.is_empty() => return Ok(None),
                Err(err) => return Err(err),
            }
        }

        Ok(Some(data))
    })
}

#[derive(Clone, Copy)]
enum ReadWait {
    SocketTimeout,
    Poll(PollBackoff),
}

impl ReadWait {
    fn after_retry(&mut self, idle_deadline: Option<Instant>, total_deadline: Option<Instant>) {
        if let Self::Poll(backoff) = self {
            sleep_until_poll(idle_deadline, total_deadline, backoff.interval);
            backoff.advance();
        }
    }

    fn on_progress(&mut self) {
        if let Self::Poll(backoff) = self {
            backoff.reset();
        }
    }
}

#[derive(Clone, Copy)]
struct PollBackoff {
    interval: Duration,
    fast_polls_remaining: u8,
}

impl PollBackoff {
    fn new() -> Self {
        Self {
            interval: STREAM_FALLBACK_POLL_INTERVAL,
            fast_polls_remaining: STREAM_FALLBACK_FAST_POLLS,
        }
    }

    fn advance(&mut self) {
        if self.fast_polls_remaining > 0 {
            self.fast_polls_remaining -= 1;
            return;
        }
        self.interval = (self.interval * 2).min(CONNECTION_POLL_INTERVAL);
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

fn with_timed_reads<T>(
    stream: &mut LocalStream,
    read: impl FnOnce(&mut LocalStream, ReadWait) -> std::io::Result<Option<T>>,
) -> std::io::Result<Option<T>> {
    match stream.set_recv_timeout(Some(CONNECTION_POLL_INTERVAL)) {
        Ok(()) => {
            let result = read(stream, ReadWait::SocketTimeout);
            finish_timed_read(result, || stream.set_recv_timeout(None))
        }
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            stream.set_nonblocking(true)?;
            let result = read(stream, ReadWait::Poll(PollBackoff::new()));
            finish_timed_read(result, || stream.set_nonblocking(false))
        }
        // A peer can disconnect after the caller's running check but before
        // setsockopt. macOS reports that closed-socket race as EINVAL.
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => Ok(None),
        Err(err) => Err(err),
    }
}

fn finish_timed_read<T>(
    result: std::io::Result<Option<T>>,
    reset: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<Option<T>> {
    match result {
        // None is terminal for this dedicated stream. macOS returns EINVAL when
        // socket options are restored after the peer has already disconnected.
        Ok(None) => Ok(None),
        Ok(value) => {
            reset()?;
            Ok(value)
        }
        Err(err) => {
            let _ = reset();
            Err(err)
        }
    }
}

fn ensure_before_deadlines(
    idle_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    message: &str,
) -> std::io::Result<()> {
    let now = Instant::now();
    if idle_deadline.is_some_and(|deadline| now >= deadline)
        || total_deadline.is_some_and(|deadline| now >= deadline)
    {
        return Err(io::Error::new(io::ErrorKind::TimedOut, message));
    }
    Ok(())
}

fn sleep_until_poll(
    idle_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    poll_interval: Duration,
) {
    let now = Instant::now();
    let until_deadline = [idle_deadline, total_deadline]
        .into_iter()
        .flatten()
        .filter_map(|deadline| deadline.checked_duration_since(now))
        .min()
        .unwrap_or(poll_interval);
    std::thread::sleep(poll_interval.min(until_deadline));
}

fn read_should_retry(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ErrorResponse, Method, ResponseResult, SuccessResponse};
    use crate::api::{ApiRequestMessage, EventHub};
    use crate::ipc::LocalStream;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    static NEXT_LOCAL_STREAM_ID: AtomicU64 = AtomicU64::new(1);

    fn local_stream_pair(_name: &str) -> (LocalStream, LocalStream, PathBuf) {
        let unique = format!(
            "hpg-{}-{}.sock",
            std::process::id(),
            NEXT_LOCAL_STREAM_ID.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, path)
    }

    fn read_response_line(stream: &mut LocalStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    fn assert_server_stream_owner(owner: &str) {
        assert!(owner.starts_with("pane.graphics.stream:"));
    }

    #[test]
    fn timed_read_skips_reset_after_stream_ends() {
        let mut reset_called = false;
        let result = finish_timed_read::<()>(Ok(None), || {
            reset_called = true;
            Ok(())
        });

        assert!(result.unwrap().is_none());
        assert!(!reset_called);
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_dispatches_binary_frames() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-pane-graphics-stream");
        client
            .write_all(br#"{"id":"stream_1","method":"pane.graphics.stream","params":{"pane_id":"pane_1"}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_1");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let ack: SuccessResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(ack.id, "stream_1");
        assert_eq!(ack.result, ResponseResult::Ok {});

        client
            .write_all(
                br#"{"format":"png","image_width":2,"image_height":1,"data_length":4,"placement":{"grid_cols":10,"grid_rows":5}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.write_all(&[1_u8, 2, 3, 4]).unwrap();
        client.flush().unwrap();

        let msg = api_rx.blocking_recv().unwrap();
        match &msg.request.method {
            Method::PaneGraphicsStreamSet(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
                assert_eq!(params.image_width, 2);
                assert_eq!(params.image_height, 1);
                assert_eq!(params.data.as_deref(), Some(&[1_u8, 2, 3, 4][..]));
                assert!(params.data_base64.is_empty());
                assert_eq!(params.placement.grid_cols, 10);
                assert_eq!(params.placement.grid_rows, 5);
            }
            other => panic!("unexpected request: {other:?}"),
        }
        msg.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: msg.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(client);
        running.store(false, Ordering::Relaxed);
        let clear = api_rx.blocking_recv().unwrap();
        match &clear.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected clear request: {other:?}"),
        }
        clear
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: clear.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_reports_open_errors_before_ack() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-pane-graphics-stream-error");
        client
            .write_all(br#"{"id":"stream_2","method":"pane.graphics.stream","params":{"pane_id":"pane_1"}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_2");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(super::super::error_response_json(
                open.request.id,
                "feature_disabled",
                "pane graphics require experimental.kitty_graphics".into(),
            ))
            .unwrap();

        let response: ErrorResponse =
            serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(response.id, "stream_2");
        assert_eq!(response.error.code, "feature_disabled");

        let close = api_rx.blocking_recv().unwrap();
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_closes_claim_after_open_timeout() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair("api-pane-graphics-stream-timeout");
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server_thread = std::thread::spawn(move || {
            serve_with_open_timeout(
                server,
                "stream_timeout".into(),
                PaneGraphicsStreamParams {
                    pane_id: "pane_1".into(),
                    owner: String::new(),
                },
                &api_tx,
                &server_running,
                Duration::from_millis(10),
            )
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_timeout");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };

        let response: ErrorResponse =
            serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(response.id, "stream_timeout");
        assert_eq!(response.error.code, "server_unavailable");
        assert!(response.error.message.contains("timed out"));

        let close = api_rx.blocking_recv().unwrap();
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(open);
        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn pane_graphics_stream_closes_claim_when_client_disconnects_before_ack() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) =
            local_stream_pair("api-pane-graphics-stream-ack-disconnect");
        client
            .write_all(br#"{"id":"stream_3","method":"pane.graphics.stream","params":{"pane_id":"pane_1"}}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        let open = api_rx.blocking_recv().unwrap();
        let stream_owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_server_stream_owner(&params.owner);
                assert_ne!(params.owner, "stream_3");
                params.owner.clone()
            }
            other => panic!("unexpected open request: {other:?}"),
        };

        drop(client);
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let (close_tx, close_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            close_tx.send(api_rx.blocking_recv()).unwrap();
        });
        let close = close_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, stream_owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[test]
    fn idle_graphics_stream_waits_for_header_without_timing_out() {
        let (_client, mut server, _path) = local_stream_pair("graphics-idle-header");
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::new(AtomicBool::new(true));
        let stop = Arc::clone(&running);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            stop.store(false, Ordering::Relaxed);
        });

        let result = read_line(
            &mut server,
            &running,
            &active,
            MAX_STREAM_FRAME_HEADER_BYTES,
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap();

        assert!(result.is_none());
        stopper.join().unwrap();
    }

    #[test]
    fn inactive_owner_cancels_idle_stream_and_dispatches_close() {
        let (mut client, server, _path) = local_stream_pair("graphics-owner-cancel");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server_thread = std::thread::spawn(move || {
            serve_with_timeouts(
                server,
                "stream-cancel".into(),
                PaneGraphicsStreamParams {
                    pane_id: "pane_1".into(),
                    owner: String::new(),
                },
                &api_tx,
                &server_running,
                Duration::from_secs(1),
                READ_TIMEOUTS,
            )
        });

        let open = api_rx.blocking_recv().unwrap();
        let owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => params.owner.clone(),
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();
        let ack: SuccessResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(ack.id, "stream-cancel");

        cancel_inactive_streams(|registered| registered != owner);

        let (close_tx, close_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || close_tx.send(api_rx.blocking_recv()).unwrap());
        let close = close_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("canceled idle stream should dispatch a close")
            .expect("API request channel should remain open");
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[test]
    fn fallback_poll_backoff_preserves_fast_window_then_reaches_poll_ceiling() {
        let mut backoff = PollBackoff::new();
        for _ in 0..STREAM_FALLBACK_FAST_POLLS {
            backoff.advance();
            assert_eq!(backoff.interval, STREAM_FALLBACK_POLL_INTERVAL);
        }

        backoff.advance();
        assert_eq!(backoff.interval, Duration::from_millis(2));
        for _ in 0..6 {
            backoff.advance();
        }
        assert_eq!(backoff.interval, CONNECTION_POLL_INTERVAL);

        backoff.reset();
        assert_eq!(backoff.interval, STREAM_FALLBACK_POLL_INTERVAL);
        assert_eq!(backoff.fast_polls_remaining, STREAM_FALLBACK_FAST_POLLS);
    }

    #[test]
    fn partial_graphics_header_times_out_after_first_byte() {
        let (mut client, mut server, _path) = local_stream_pair("graphics-partial-header");
        client.write_all(b"{").unwrap();
        client.flush().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::new(AtomicBool::new(true));

        let error = read_line(
            &mut server,
            &running,
            &active,
            MAX_STREAM_FRAME_HEADER_BYTES,
            Duration::from_millis(20),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn trickled_graphics_body_obeys_absolute_deadline() {
        let (mut client, mut server, _path) = local_stream_pair("graphics-trickle-body");
        client.write_all(&[1_u8]).unwrap();
        client.flush().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::new(AtomicBool::new(true));
        let writer_running = Arc::clone(&running);
        let writer = std::thread::spawn(move || {
            while writer_running.load(Ordering::Relaxed) {
                if client.write_all(&[1_u8]).is_err() {
                    break;
                }
                let _ = client.flush();
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let started = Instant::now();
        let error = read_exact(
            &mut server,
            1024,
            &running,
            &active,
            Duration::from_millis(20),
            Duration::from_millis(60),
        )
        .unwrap_err();

        running.store(false, Ordering::Relaxed);
        writer.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn timed_out_header_dispatches_owner_scoped_stream_close() {
        let (mut client, server, _path) = local_stream_pair("graphics-timeout-close");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server_thread = std::thread::spawn(move || {
            serve_with_timeouts(
                server,
                "stream-timeout".into(),
                PaneGraphicsStreamParams {
                    pane_id: "pane_1".into(),
                    owner: String::new(),
                },
                &api_tx,
                &server_running,
                Duration::from_secs(1),
                ReadTimeouts {
                    header_idle: Duration::from_millis(20),
                    header_total: Duration::from_millis(100),
                    body_idle: Duration::from_millis(20),
                    body_total: Duration::from_millis(100),
                },
            )
        });

        let open = api_rx.blocking_recv().unwrap();
        let owner = match &open.request.method {
            Method::PaneGraphicsStreamOpen(params) => params.owner.clone(),
            other => panic!("unexpected open request: {other:?}"),
        };
        open.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: open.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();
        let ack: SuccessResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(ack.id, "stream-timeout");
        client.write_all(b"{").unwrap();
        client.flush().unwrap();

        let (close_tx, close_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || close_tx.send(api_rx.blocking_recv()).unwrap());
        let close = close_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        match &close.request.method {
            Method::PaneGraphicsStreamClose(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.owner, owner);
            }
            other => panic!("unexpected close request: {other:?}"),
        }
        close
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: close.request.id,
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let error = server_thread.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn oversized_stream_frame_is_rejected_before_body_or_app_dispatch() {
        let (mut client, mut server, _path) = local_stream_pair("graphics-oversized-frame");
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let stream_active = Arc::new(AtomicBool::new(true));
        let server_thread = std::thread::spawn(move || {
            serve_frames(
                &mut server,
                "stream-oversized",
                "owner-1",
                "pane_1",
                &api_tx,
                &server_running,
                &stream_active,
                READ_TIMEOUTS,
            )
        });
        let header = serde_json::json!({
            "format": "png",
            "image_width": 1,
            "image_height": 1,
            "data_length": crate::api::schema::PANE_GRAPHICS_STREAM_MAX_BYTES + 1,
        });
        client.write_all(format!("{header}\n").as_bytes()).unwrap();
        client.flush().unwrap();

        let response: ErrorResponse =
            serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(response.error.code, "image_too_large");
        assert!(server_thread.join().unwrap().is_ok());
        assert!(api_rx.try_recv().is_err());
    }
}
