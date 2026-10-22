//! Persistent, ordered, client→daemon input channel (`pane.input.stream`).
//!
//! The write-side mirror of `pane.stream` (which is the persistent read
//! firehose). A client opens one held-open connection per attached pane and
//! writes newline-delimited input frames. A text frame (raw terminal bytes — the
//! client's keystroke stream) is dispatched as `pane.send_text` so control bytes
//! reach the PTY RAW; a frame carrying named `keys` is dispatched as
//! `pane.send_input` for its key encoding. Both inherit the composer/turn-abort
//! bookkeeping. (Routing text through `send_input` would bracketed-paste-wrap it
//! and break control keys under DECSET 2004 — see the dispatch in `serve_frames`.)
//!
//! This removes the per-keystroke remote process and connection setup that
//! dominates felt typing latency on the iPad hardware-keyboard path (issue #62):
//! keystrokes become socket writes on one long-lived ordered channel, which also
//! preserves order by construction.
//!
//! The line-framing read machinery lives in `stream_read`, shared with
//! `gram_upload_stream.rs`; only this channel's own byte cap and deadlines are
//! set here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneInputStreamParams, PaneSendInputParams,
    PaneSendTextParams, Request, ResponseResult, SuccessResponse,
};
use crate::api::{ApiRequestSender, ApiStream};
use crate::ipc::is_connection_closed_error;

use super::stream_read::{read_line, stream_is_running};
use super::{
    api_response_outcome, dispatch_to_app_with_timeout, write_json_line,
    write_json_line_allow_disconnect, write_text_line_allow_disconnect, APP_RESPONSE_TIMEOUT,
};

// A whole input frame (incl. JSON encoding of pasted text) is one line. Large
// pastes are split client-side; a frame past this cap is a protocol error.
const MAX_INPUT_FRAME_BYTES: usize = 1024 * 1024;
// Keystroke frames are tiny and near-instant to enqueue, but the channel is held
// open for a human's whole session, so idle waits between frames are unbounded by
// design — only a mid-frame stall (a partially written line) is deadlined.
const INPUT_FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const INPUT_FRAME_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

/// One inbound input frame. A text-only frame dispatches as `pane.send_text`
/// (raw bytes); a frame carrying named `keys` dispatches as `pane.send_input`.
#[derive(serde::Deserialize)]
struct InputFrame {
    seq: u64,
    #[serde(default)]
    text: String,
    #[serde(default)]
    keys: Vec<String>,
    /// When true, the server replies with an `{"seq":N,"ok":true}` line after the
    /// frame is accepted. Default false (fire-and-forget) — the real confirmation
    /// for a human is the echo on `pane.stream`.
    #[serde(default)]
    ack: bool,
}

#[derive(serde::Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
struct InputAck {
    seq: u64,
    ok: bool,
}

#[derive(serde::Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
struct InputFrameError {
    seq: u64,
    error: ErrorBody,
}

pub(super) fn serve(
    stream: ApiStream,
    request_id: String,
    params: PaneInputStreamParams,
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
    mut stream: ApiStream,
    request_id: String,
    params: PaneInputStreamParams,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    open_timeout: Duration,
) -> std::io::Result<()> {
    let pane_id = params.pane_id.clone();
    let stream_active = Arc::new(AtomicBool::new(true));

    // Open handshake: validate the pane exists (and reject an unknown pane) before
    // upgrading the connection into a held-open frame loop.
    let open_response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneInputStreamOpen(params),
        },
        api_tx,
        Some(open_timeout),
    );
    if api_response_outcome(&open_response) != "ok" {
        stream_active.store(false, Ordering::Release);
        return write_text_line_allow_disconnect(&mut stream, &open_response);
    }

    if let Err(err) = write_json_line(
        &mut stream,
        &SuccessResponse {
            id: request_id.clone(),
            result: ResponseResult::Ok {},
        },
    ) {
        stream_active.store(false, Ordering::Release);
        if is_connection_closed_error(&err) {
            return Ok(());
        }
        return Err(err);
    }

    let result = serve_frames(
        &mut stream,
        &request_id,
        &pane_id,
        api_tx,
        running,
        &stream_active,
    );
    stream_active.store(false, Ordering::Release);
    result
}

fn serve_frames(
    stream: &mut ApiStream,
    request_id: &str,
    pane_id: &str,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
    stream_active: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    let mut last_seq: Option<u64> = None;
    while stream_is_running(running, stream_active) {
        let Some(line) = read_line(
            stream,
            running,
            stream_active,
            MAX_INPUT_FRAME_BYTES,
            INPUT_FRAME_IDLE_TIMEOUT,
            INPUT_FRAME_TOTAL_TIMEOUT,
            "input frame",
        )?
        else {
            return Ok(());
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let frame = match serde_json::from_str::<InputFrame>(line) {
            Ok(frame) => frame,
            Err(err) => {
                write_json_line_allow_disconnect(
                    stream,
                    &ErrorResponse {
                        id: request_id.to_string(),
                        error: ErrorBody {
                            code: "invalid_frame".into(),
                            message: format!("invalid input frame: {err}"),
                        },
                    },
                )?;
                return Ok(());
            }
        };

        // Strictly increasing seq is a cheap ordering tripwire: a gap, a repeat,
        // or a decrease means a client bug or a reordered channel — fail loudly.
        if last_seq.is_some_and(|prev| frame.seq <= prev) {
            write_json_line_allow_disconnect(
                stream,
                &InputFrameError {
                    seq: frame.seq,
                    error: ErrorBody {
                        code: "invalid_sequence".into(),
                        message: "seq must be strictly increasing".into(),
                    },
                },
            )?;
            return Ok(());
        }
        last_seq = Some(frame.seq);

        let ack = frame.ack;
        let seq = frame.seq;
        // Raw terminal bytes (the client's keystroke stream) go through send_text
        // so control bytes — backspace, ctrl-c, shift+tab, arrows — reach the PTY
        // RAW. send_input would run them through encode_api_text, which
        // bracketed-paste-wraps (\x1b[200~…\x1b[201~) whenever the pane has DECSET
        // 2004 on (interactive shells, editors, Claude Code), turning a control
        // byte into inert paste content instead of a key action (issue #62). Only
        // frames that carry named `keys` need the send_input encoding path.
        let method = if frame.keys.is_empty() {
            Method::PaneSendText(PaneSendTextParams {
                pane_id: pane_id.to_string(),
                text: frame.text,
            })
        } else {
            Method::PaneSendInput(PaneSendInputParams {
                pane_id: pane_id.to_string(),
                text: frame.text,
                keys: frame.keys,
            })
        };
        let response = dispatch_to_app_with_timeout(
            Request {
                id: format!("{request_id}:frame:{seq}"),
                method,
            },
            api_tx,
            Some(APP_RESPONSE_TIMEOUT),
        );

        if api_response_outcome(&response) == "ok" {
            if ack {
                write_json_line_allow_disconnect(stream, &InputAck { seq, ok: true })?;
            }
        } else {
            // A dispatch failure (pane gone, backpressure timeout, invalid key) is
            // terminal for the channel: surface it and close.
            let error = serde_json::from_str::<ErrorResponse>(&response)
                .map(|parsed| parsed.error)
                .unwrap_or_else(|_| ErrorBody {
                    code: "pane_send_failed".into(),
                    message: "input frame was not accepted".into(),
                });
            write_json_line_allow_disconnect(stream, &InputFrameError { seq, error })?;
            return Ok(());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // The socket-driven tests and their helpers are unix-only; the frame-default test
    // runs everywhere.
    #[cfg(unix)]
    use crate::api::schema::{ErrorResponse, Method, ResponseResult, SuccessResponse};
    #[cfg(unix)]
    use crate::api::ApiRequestMessage;
    #[cfg(unix)]
    use crate::api::EventHub;
    #[cfg(unix)]
    use crate::ipc::LocalStream;
    #[cfg(unix)]
    use interprocess::local_socket::traits::Listener as _;
    #[cfg(unix)]
    use std::io::{BufRead, BufReader, Write};
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    #[cfg(unix)]
    use std::sync::Arc;
    #[cfg(unix)]
    use tokio::sync::mpsc;

    #[cfg(unix)]
    static NEXT_LOCAL_STREAM_ID: AtomicU64 = AtomicU64::new(1);

    #[cfg(unix)]
    fn local_stream_pair() -> (LocalStream, LocalStream, PathBuf) {
        let unique = format!(
            "hpi-{}-{}.sock",
            std::process::id(),
            NEXT_LOCAL_STREAM_ID.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, path)
    }

    #[cfg(unix)]
    fn read_response_line(stream: &mut LocalStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    #[cfg(unix)]
    fn respond_ok(message: ApiRequestMessage) {
        let response = serde_json::to_string(&SuccessResponse {
            id: message.request.id,
            result: ResponseResult::Ok {},
        })
        .unwrap();
        message.respond_to.send(response).unwrap();
    }

    #[cfg(unix)]
    fn respond_error(message: ApiRequestMessage, code: &str, msg: &str) {
        let response = serde_json::to_string(&ErrorResponse {
            id: message.request.id,
            error: ErrorBody {
                code: code.into(),
                message: msg.into(),
            },
        })
        .unwrap();
        message.respond_to.send(response).unwrap();
    }

    /// Drive the open handshake: write the open line, answer the app's
    /// `PaneInputStreamOpen` with ok, and consume the success ack line.
    #[cfg(unix)]
    fn open_ok(
        client: &mut LocalStream,
        api_rx: &mut mpsc::UnboundedReceiver<ApiRequestMessage>,
        pane_id: &str,
    ) {
        client
            .write_all(
                format!(
                    r#"{{"id":"in_1","method":"pane.input.stream","params":{{"pane_id":"{pane_id}"}}}}"#
                )
                .as_bytes(),
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let open = api_rx.blocking_recv().unwrap();
        match &open.request.method {
            Method::PaneInputStreamOpen(params) => assert_eq!(params.pane_id, pane_id),
            other => panic!("unexpected open request: {other:?}"),
        }
        respond_ok(open);

        let ack: SuccessResponse = serde_json::from_str(&read_response_line(client)).unwrap();
        assert_eq!(ack.id, "in_1");
        assert_eq!(ack.result, ResponseResult::Ok {});
    }

    #[cfg(unix)]
    #[test]
    fn dispatches_text_as_send_text_keys_as_send_input_with_sparse_acks() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        open_ok(&mut client, &mut api_rx, "pane_1");

        // Fire-and-forget TEXT frame: dispatched as pane.send_text (RAW bytes, so
        // control keys are NOT bracketed-paste-wrapped), no reply line.
        client.write_all(br#"{"seq":1,"text":"a"}"#).unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();
        let frame1 = api_rx.blocking_recv().unwrap();
        match &frame1.request.method {
            Method::PaneSendText(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert_eq!(params.text, "a");
            }
            other => panic!("unexpected frame request: {other:?}"),
        }
        respond_ok(frame1);

        // Acked frame: dispatched, then a `{seq, ok:true}` line comes back.
        client
            .write_all(br#"{"seq":2,"keys":["enter"],"ack":true}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();
        let frame2 = api_rx.blocking_recv().unwrap();
        match &frame2.request.method {
            Method::PaneSendInput(params) => {
                assert_eq!(params.pane_id, "pane_1");
                assert!(params.text.is_empty());
                assert_eq!(params.keys, vec!["enter".to_string()]);
            }
            other => panic!("unexpected frame request: {other:?}"),
        }
        respond_ok(frame2);

        let ack: InputAck = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(ack.seq, 2);
        assert!(ack.ok);

        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn reports_open_error_before_ack_for_unknown_pane() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        client
            .write_all(
                br#"{"id":"in_1","method":"pane.input.stream","params":{"pane_id":"ghost"}}"#,
            )
            .unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let open = api_rx.blocking_recv().unwrap();
        respond_error(open, "pane_not_found", "no such pane");

        let err: ErrorResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(err.id, "in_1");
        assert_eq!(err.error.code, "pane_not_found");

        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_increasing_seq_and_closes() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        open_ok(&mut client, &mut api_rx, "pane_1");

        client.write_all(br#"{"seq":5,"text":"a"}"#).unwrap();
        client.write_all(b"\n").unwrap();
        client.write_all(br#"{"seq":5,"text":"b"}"#).unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        // First frame dispatches normally.
        respond_ok(api_rx.blocking_recv().unwrap());

        // Second frame repeats seq 5 -> rejected, channel closes.
        let err: InputFrameError = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(err.seq, 5);
        assert_eq!(err.error.code, "invalid_sequence");

        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
        assert!(
            api_rx.try_recv().is_err(),
            "no dispatch for the rejected frame"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_malformed_frame_and_closes() {
        let (api_tx, mut api_rx) = mpsc::unbounded_channel::<ApiRequestMessage>();
        let (mut client, server, _path) = local_stream_pair();

        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let event_hub = EventHub::default();
        let server_thread = std::thread::spawn(move || {
            super::super::handle_connection(server, &api_tx, &event_hub, &server_running, None)
        });

        open_ok(&mut client, &mut api_rx, "pane_1");

        client.write_all(b"not a json frame\n").unwrap();
        client.flush().unwrap();

        let err: ErrorResponse = serde_json::from_str(&read_response_line(&mut client)).unwrap();
        assert_eq!(err.error.code, "invalid_frame");

        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
        assert!(
            api_rx.try_recv().is_err(),
            "no dispatch for the malformed frame"
        );
    }

    #[test]
    fn input_frame_defaults_are_send_input_compatible() {
        let frame: InputFrame = serde_json::from_str(r#"{"seq":7,"text":"x"}"#).unwrap();
        assert_eq!(frame.seq, 7);
        assert_eq!(frame.text, "x");
        assert!(frame.keys.is_empty());
        assert!(!frame.ack);

        let keys_only: InputFrame =
            serde_json::from_str(r#"{"seq":8,"keys":["ctrl+c"],"ack":true}"#).unwrap();
        assert_eq!(keys_only.seq, 8);
        assert!(keys_only.text.is_empty());
        assert_eq!(keys_only.keys, vec!["ctrl+c".to_string()]);
        assert!(keys_only.ack);
    }
}
