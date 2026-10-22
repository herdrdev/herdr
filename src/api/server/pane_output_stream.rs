//! Outbound `pane.stream` server: a persistent server->client raw PTY byte
//! firehose. Unlike the inbound streams (`pane_input_stream`,
//! `gram_upload_stream`) it never reads frames from the client: it drains a
//! pane's bounded [`OutputRing`] and writes newline-delimited JSON frames.
//!
//! Lifecycle: dispatch `PaneStreamOpen` to the app (validate + attach + publish
//! the ring), look the ring up off the app loop, write the `stream_started` ack
//! and a `reset` seed, then a Condvar-woken drain loop that coalesces ready
//! bytes into `data` frames, interleaves `resize`, heartbeats with `ping`, and
//! emits `exited` when the runtime is gone. On exit it dispatches
//! `PaneStreamClose` to release the viewer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;

use crate::api::output_registry;
use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneStreamParams, Request, ResponseResult, SuccessResponse,
};
use crate::api::{ApiRequestSender, ApiStream};
use crate::ipc::is_connection_closed_error;
use crate::pane::{clamp_max_frame_bytes, OutputDrain, OutputSnapshot, OutputWait};

use super::{
    api_response_outcome, dispatch_to_app_with_timeout, should_stop_connection, write_json_line,
    write_text_line_allow_disconnect, APP_RESPONSE_TIMEOUT,
};

/// Idle heartbeat / dead-peer reap cadence.
const PING_INTERVAL: Duration = Duration::from_secs(20);

const STREAM_TAG: &str = "pane.bytes";

/// Most scrollback rows one keyframe carries; the same ceiling `pane.read`
/// applies to `recent`, so a keyframe stays a bounded single line.
const MAX_KEYFRAME_HISTORY_LINES: usize = 1000;

/// Scrollback rows each `reset` keyframe should carry: `scrollback_lines`,
/// clamped, unless the client opted out with `include_history: false`.
fn keyframe_history_lines(params: &PaneStreamParams) -> usize {
    if !params.include_history {
        return 0;
    }
    params
        .scrollback_lines
        .unwrap_or(0)
        .min(MAX_KEYFRAME_HISTORY_LINES)
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One newline-delimited `pane.stream` frame line.
#[derive(serde::Serialize)]
struct StreamFrameLine {
    stream: &'static str,
    frame: &'static str,
    seq: u64,
    epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cols: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_b64: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    lagged: bool,
    /// Base64 ANSI scrollback rows above a `reset` keyframe's screen, oldest
    /// first, captured atomically with it. Present only when the stream asked
    /// for `scrollback_lines`; empty when the pane has no history to give.
    #[serde(skip_serializing_if = "Option::is_none")]
    history_b64: Option<String>,
    /// `pane.history` cursor for the first row of `history_b64`; present only
    /// when older rows remain above it.
    #[serde(skip_serializing_if = "Option::is_none")]
    history_cursor: Option<String>,
}

impl StreamFrameLine {
    fn base(frame: &'static str, seq: u64, epoch: u64) -> Self {
        Self {
            stream: STREAM_TAG,
            frame,
            seq,
            epoch,
            cols: None,
            rows: None,
            data_b64: None,
            lagged: false,
            history_b64: None,
            history_cursor: None,
        }
    }

    fn reset(seq: u64, epoch: u64, seed: &OutputSnapshot, lagged: bool) -> Self {
        Self {
            cols: Some(seed.cols),
            rows: Some(seed.rows),
            data_b64: Some(encode_bytes(seed.ansi.as_bytes())),
            history_b64: seed
                .history
                .as_deref()
                .map(|history| encode_bytes(history.as_bytes())),
            history_cursor: seed.history_cursor.clone(),
            lagged,
            ..Self::base("reset", seq, epoch)
        }
    }

    fn data(seq: u64, epoch: u64, data_b64: String) -> Self {
        Self {
            data_b64: Some(data_b64),
            ..Self::base("data", seq, epoch)
        }
    }

    fn resize(seq: u64, epoch: u64, cols: u16, rows: u16) -> Self {
        Self {
            cols: Some(cols),
            rows: Some(rows),
            ..Self::base("resize", seq, epoch)
        }
    }

    fn ping(seq: u64, epoch: u64) -> Self {
        Self::base("ping", seq, epoch)
    }

    fn exited(seq: u64, epoch: u64) -> Self {
        Self::base("exited", seq, epoch)
    }
}

fn encode_bytes(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(super) fn serve(
    stream: ApiStream,
    request_id: String,
    params: PaneStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    serve_watched(stream, request_id, params, api_tx, running, None)
}

/// Called before every frame, on idle ticks, and with `closed = true` when the
/// runtime ended. A returned line is written as the final frame and the
/// stream closes.
pub(super) type StreamWatch<'a> = &'a mut dyn FnMut(bool) -> Option<String>;

/// Poll cadence for a watched stream, so a watch fires promptly while idle.
const WATCH_POLL: Duration = Duration::from_millis(250);

/// `pane.stream` with an optional watch (the guest gate's pause/revoke check).
pub(super) fn serve_watched(
    mut stream: ApiStream,
    request_id: String,
    params: PaneStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    watch: Option<StreamWatch<'_>>,
) -> std::io::Result<()> {
    let pane_id = params.pane_id.clone();
    // Capture the viewer id from the OPEN params before they are moved into the
    // open request, so the synthesized close can drop exactly this viewer's width
    // lease (#137). The close params are built fresh below and would otherwise
    // carry no viewer id.
    let viewer_id = params.viewer_id.clone();
    let max_frame_bytes = clamp_max_frame_bytes(params.max_frame_bytes);
    let history_lines = keyframe_history_lines(&params);

    // Validate + attach + publish the ring on the app loop.
    let open_response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneStreamOpen(params),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );

    // The open request has been *sent* to the app queue. Even if we timed out
    // waiting for its reply, the app may still process it later and attach a
    // viewer/ring. So on EVERY exit path — including a failed or timed-out open —
    // dispatch a matching close. Both messages travel the same FIFO channel, so
    // a late open is always followed by this close and cannot leak an attach. On
    // a genuine synchronous failure (e.g. pane_not_found) no ring was created and
    // the close is a harmless no-op.
    let result = if api_response_outcome(&open_response) == "ok" {
        serve_attached(
            &mut stream,
            &request_id,
            &pane_id,
            max_frame_bytes,
            history_lines,
            running,
            watch,
        )
    } else {
        write_text_line_allow_disconnect(&mut stream, &open_response)
    };
    dispatch_close(&pane_id, viewer_id, api_tx);
    result
}

fn serve_attached(
    stream: &mut ApiStream,
    request_id: &str,
    pane_id: &str,
    max_frame_bytes: usize,
    history_lines: usize,
    running: &Arc<AtomicBool>,
    mut watch: Option<StreamWatch<'_>>,
) -> std::io::Result<()> {
    let Some(ring) = output_registry::lookup(pane_id) else {
        // The ring vanished between attach and lookup (pane closed). Report it
        // rather than silently hanging.
        return write_json_line(
            stream,
            &ErrorResponse {
                id: request_id.to_string(),
                error: ErrorBody {
                    code: "pane_not_found".into(),
                    message: format!("pane {pane_id} has no live output stream"),
                },
            },
        )
        .or_else(swallow_disconnect);
    };

    let epoch = ring.epoch();
    let Some(seed) = ring.snapshot(history_lines) else {
        // The runtime is already gone; tell the client and close.
        return finish_exited(stream, &mut watch, 0, epoch);
    };

    // stream_started ack carries the geometry the client lacks today.
    if !emit_watched(
        stream,
        &mut watch,
        &SuccessResponse {
            id: request_id.to_string(),
            result: ResponseResult::StreamStarted {
                pane_id: pane_id.to_string(),
                epoch,
                cols: seed.cols,
                rows: seed.rows,
                base_seq: seed.cursor,
                resync: true,
            },
        },
    )? {
        return Ok(());
    }

    // reset seed: base64 full-screen ANSI at exactly `base_seq`, with the
    // requested scrollback captured in the same critical section.
    if !emit_watched(
        stream,
        &mut watch,
        &StreamFrameLine::reset(seed.cursor, epoch, &seed, false),
    )? {
        return Ok(());
    }

    let mut cursor = seed.cursor;
    let mut resize_id = seed.resize_id;

    let wait = if watch.is_some() {
        WATCH_POLL
    } else {
        PING_INTERVAL
    };
    let mut last_ping = std::time::Instant::now();
    loop {
        if !running.load(Ordering::Relaxed) {
            return Ok(());
        }
        match ring.wait_for_activity(cursor, resize_id, wait) {
            OutputWait::Closed => {
                return finish_exited(stream, &mut watch, cursor, epoch);
            }
            OutputWait::Idle => {
                // On the idle tick, reap a dead peer before heartbeating.
                if should_stop_connection(stream, running)? {
                    return Ok(());
                }
                // An idle watched stream still closes promptly on pause or revoke.
                if let Some(line) = watch.as_mut().and_then(|watch| watch(false)) {
                    return write_text_line_allow_disconnect(stream, &line);
                }
                if watch.is_none() || last_ping.elapsed() >= PING_INTERVAL {
                    last_ping = std::time::Instant::now();
                    if !emit_watched(stream, &mut watch, &StreamFrameLine::ping(cursor, epoch))? {
                        return Ok(());
                    }
                }
            }
            OutputWait::Ready => match ring.drain(cursor, resize_id, max_frame_bytes) {
                OutputDrain::Lagged => {
                    // Snapshot-collapse resync: discard the backlog, re-seed one
                    // full-screen keyframe, and jump the cursor to the live edge.
                    // It carries history too: the bytes dropped from the backlog
                    // may have scrolled into it.
                    let Some(seed) = ring.snapshot(history_lines) else {
                        return finish_exited(stream, &mut watch, cursor, epoch);
                    };
                    if !emit_watched(
                        stream,
                        &mut watch,
                        &StreamFrameLine::reset(seed.cursor, epoch, &seed, true),
                    )? {
                        return Ok(());
                    }
                    cursor = seed.cursor;
                    resize_id = seed.resize_id;
                }
                OutputDrain::Resize {
                    cols,
                    rows,
                    resize_id: next_resize_id,
                } => {
                    if !emit_watched(
                        stream,
                        &mut watch,
                        &StreamFrameLine::resize(cursor, epoch, cols, rows),
                    )? {
                        return Ok(());
                    }
                    resize_id = next_resize_id;
                }
                OutputDrain::Data {
                    chunks,
                    cursor: next_cursor,
                } => {
                    // Concatenate the shared `Bytes` slices and base64-encode the
                    // frame OUTSIDE the ring lock (drain already released it), so
                    // the reader thread's `append` is never blocked behind framing.
                    let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
                    let mut payload = Vec::with_capacity(total);
                    for chunk in &chunks {
                        payload.extend_from_slice(chunk);
                    }
                    if !emit_watched(
                        stream,
                        &mut watch,
                        &StreamFrameLine::data(cursor, epoch, encode_bytes(&payload)),
                    )? {
                        return Ok(());
                    }
                    cursor = next_cursor;
                }
                OutputDrain::Idle => {}
            },
        }
    }
}

/// End the stream because the runtime ended. A watched stream reports it
/// through its watch instead of an `exited` frame.
fn finish_exited(
    stream: &mut ApiStream,
    watch: &mut Option<StreamWatch<'_>>,
    cursor: u64,
    epoch: u64,
) -> std::io::Result<()> {
    if let Some(line) = watch.as_mut().and_then(|watch| watch(true)) {
        return write_text_line_allow_disconnect(stream, &line);
    }
    emit(stream, &StreamFrameLine::exited(cursor, epoch)).map(|_| ())
}

/// [`emit`] after asking the watch whether the stream may still send. Every
/// frame is checked, so no output follows a pause or revoke; a refusal writes
/// the watch's line and reports `false` to stop.
fn emit_watched<T: serde::Serialize>(
    stream: &mut ApiStream,
    watch: &mut Option<StreamWatch<'_>>,
    value: &T,
) -> std::io::Result<bool> {
    if let Some(line) = watch.as_mut().and_then(|watch| watch(false)) {
        write_text_line_allow_disconnect(stream, &line)?;
        return Ok(false);
    }
    emit(stream, value)
}

/// Write one frame/response line, reporting `false` when the peer has closed so
/// the caller can stop cleanly. A wedged socket write times out via the
/// connection-wide send timeout and tears down only this connection.
fn emit<T: serde::Serialize>(stream: &mut ApiStream, value: &T) -> std::io::Result<bool> {
    match write_json_line(stream, value) {
        Ok(()) => Ok(true),
        Err(err) if is_connection_closed_error(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

fn swallow_disconnect(err: std::io::Error) -> std::io::Result<()> {
    if is_connection_closed_error(&err) {
        Ok(())
    } else {
        Err(err)
    }
}

fn dispatch_close(pane_id: &str, viewer_id: Option<String>, api_tx: &ApiRequestSender) {
    let _response = dispatch_to_app_with_timeout(
        Request {
            id: format!("pane.stream.close:{pane_id}"),
            method: Method::PaneStreamClose(PaneStreamParams {
                pane_id: pane_id.to_string(),
                include_history: true,
                resume_from: None,
                epoch: None,
                max_frame_bytes: None,
                scrollback_lines: None,
                // Threaded from the open so the close drops this viewer's lease.
                viewer_id,
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(history: Option<&str>) -> OutputSnapshot {
        OutputSnapshot {
            ansi: "seed".into(),
            history: history.map(str::to_string),
            history_cursor: None,
            cursor: 918_273,
            resize_id: 0,
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn reset_frame_shape_includes_geometry_and_omits_lagged_when_false() {
        let frame = StreamFrameLine::reset(918_273, 7, &seed(None), false);
        let value = serde_json::to_value(&frame).unwrap();
        assert_eq!(value["stream"], "pane.bytes");
        assert_eq!(value["frame"], "reset");
        assert_eq!(value["seq"], 918_273);
        assert_eq!(value["epoch"], 7);
        assert_eq!(value["cols"], 80);
        assert_eq!(value["rows"], 24);
        assert_eq!(value["data_b64"], "c2VlZA==");
        assert!(value.get("lagged").is_none());
        // No history requested: the frame is byte-for-byte what old clients parse.
        assert!(value.get("history_b64").is_none());
        assert!(value.get("history_cursor").is_none());
    }

    #[test]
    fn reset_frame_carries_requested_history_even_when_empty() {
        let value =
            serde_json::to_value(StreamFrameLine::reset(1, 7, &seed(Some("old")), false)).unwrap();
        assert_eq!(value["history_b64"], "b2xk");
        let value =
            serde_json::to_value(StreamFrameLine::reset(1, 7, &seed(Some("")), false)).unwrap();
        assert_eq!(
            value["history_b64"], "",
            "asked-for-but-empty must stay distinguishable from not-sent"
        );
    }

    #[test]
    fn reset_frame_carries_history_cursor_only_when_older_rows_exist() {
        let value =
            serde_json::to_value(StreamFrameLine::reset(1, 7, &seed(Some("old")), false)).unwrap();
        assert!(
            value.get("history_cursor").is_none(),
            "history reaching the start of scrollback offers no paging"
        );

        let mut paged = seed(Some("old"));
        paged.history_cursor = Some("tok".into());
        let value = serde_json::to_value(StreamFrameLine::reset(1, 7, &paged, false)).unwrap();
        assert_eq!(value["history_b64"], "b2xk");
        assert_eq!(value["history_cursor"], "tok");
    }

    #[test]
    fn keyframe_history_lines_follow_the_request_and_are_bounded() {
        let params = |include_history, scrollback_lines| PaneStreamParams {
            pane_id: "p".into(),
            include_history,
            resume_from: None,
            epoch: None,
            max_frame_bytes: None,
            scrollback_lines,
            viewer_id: None,
        };
        assert_eq!(keyframe_history_lines(&params(true, None)), 0);
        assert_eq!(keyframe_history_lines(&params(true, Some(400))), 400);
        assert_eq!(
            keyframe_history_lines(&params(true, Some(50_000))),
            MAX_KEYFRAME_HISTORY_LINES
        );
        assert_eq!(keyframe_history_lines(&params(false, Some(400))), 0);
    }

    #[test]
    fn lagged_reset_sets_flag() {
        let frame = StreamFrameLine::reset(1, 7, &seed(None), true);
        let value = serde_json::to_value(&frame).unwrap();
        assert_eq!(value["lagged"], true);
    }

    #[test]
    fn data_frame_carries_only_payload() {
        let frame = StreamFrameLine::data(42, 7, encode_bytes(b"hi"));
        let value = serde_json::to_value(&frame).unwrap();
        assert_eq!(value["frame"], "data");
        assert_eq!(value["seq"], 42);
        assert_eq!(value["data_b64"], "aGk=");
        assert!(value.get("cols").is_none());
        assert!(value.get("rows").is_none());
    }

    #[test]
    fn resize_and_control_frames_shape() {
        let resize = serde_json::to_value(StreamFrameLine::resize(5, 7, 100, 30)).unwrap();
        assert_eq!(resize["frame"], "resize");
        assert_eq!(resize["cols"], 100);
        assert_eq!(resize["rows"], 30);
        assert!(resize.get("data_b64").is_none());

        let ping = serde_json::to_value(StreamFrameLine::ping(9, 7)).unwrap();
        assert_eq!(ping["frame"], "ping");
        assert!(ping.get("data_b64").is_none());

        let exited = serde_json::to_value(StreamFrameLine::exited(9, 7)).unwrap();
        assert_eq!(exited["frame"], "exited");
    }
}
