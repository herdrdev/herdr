//! Stdin input reading for the thin client.
//!
//! On Unix, reads stdin bytes and forwards framed input to the main event loop.
//! The server handles semantic parsing. On Windows, crossterm may surface
//! terminal control strings as character key events, so the reader re-frames
//! those control bytes before forwarding semantic client input events.
//!
//! This is simpler and more reliable because:
//! - The server has the same input parsing code
//! - We avoid duplicating parsing logic in the client
//! - Host terminal control replies can be buffered or discarded before they leak

#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;

#[cfg(unix)]
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use tokio::sync::mpsc;

use super::ClientLoopEvent;

#[cfg(any(windows, test))]
pub(super) mod windows_vti;

// ---------------------------------------------------------------------------
// Stdin reader thread
// ---------------------------------------------------------------------------

/// Reads raw bytes from stdin and sends them to the main event loop.
///
/// This runs on a dedicated thread because stdin reading is blocking.
/// The main loop receives the raw bytes and forwards them as
/// `ClientMessage::Input` to the server.
pub fn stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
    host_theme_query_pending: Arc<AtomicU32>,
    host_cell_size_query_sent: bool,
    host_mouse_capture_active: Arc<AtomicBool>,
    host_sgr_pixels_active: Arc<AtomicBool>,
    host_escape_disambiguation_active: bool,
    initial_host_input: Vec<u8>,
    #[cfg(unix)] direct_response: Arc<std::sync::Mutex<super::direct_graphics::ResponseMatcher>>,
    #[cfg(unix)] direct_response_active: Arc<AtomicBool>,
) {
    #[cfg(windows)]
    {
        let _ = (
            host_theme_query_pending,
            host_cell_size_query_sent,
            host_mouse_capture_active,
            host_sgr_pixels_active,
        );
        let _ = (host_escape_disambiguation_active, initial_host_input);
        windows_stdin_reader_loop(event_tx, should_quit, host_color_query_sent);
    }

    #[cfg(unix)]
    unix_stdin_reader_loop(
        event_tx,
        should_quit,
        host_color_query_sent,
        host_theme_query_pending,
        host_cell_size_query_sent,
        host_mouse_capture_active,
        host_sgr_pixels_active,
        host_escape_disambiguation_active,
        initial_host_input,
        direct_response,
        direct_response_active,
    );
}

#[cfg(unix)]
fn unix_stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
    host_theme_query_pending: Arc<AtomicU32>,
    host_cell_size_query_sent: bool,
    host_mouse_capture_active: Arc<AtomicBool>,
    host_sgr_pixels_active: Arc<AtomicBool>,
    host_escape_disambiguation_active: bool,
    initial_host_input: Vec<u8>,
    direct_response: Arc<std::sync::Mutex<super::direct_graphics::ResponseMatcher>>,
    direct_response_active: Arc<AtomicBool>,
) {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut scratch = [0u8; 4096];
    let mut framer = crate::raw_input::RawInputByteFramer::for_host_input();
    framer.set_host_escape_disambiguation_active(host_escape_disambiguation_active);
    if host_color_query_sent {
        framer.host_color_query_sent();
        framer.enable_host_color_scheme_change_tracking();
        framer.enable_host_appearance_query_on_focus();
    }
    if host_cell_size_query_sent {
        framer.host_cell_size_query_sent();
    }
    let mut pending_palette = Vec::new();
    let mut pending_mode = None;
    let mut last_geometry = None;
    let mut direct_filter = super::direct_graphics::InputFilter::default();

    if !initial_host_input.is_empty() {
        let sgr_pixels = host_sgr_pixels_active.load(Ordering::Acquire);
        if sgr_pixels {
            last_geometry = crate::input::mouse::HostGeometry::current();
        }
        let chunks = framer.push(&initial_host_input);
        if !send_unix_input_chunks(
            chunks,
            &event_tx,
            &mut pending_palette,
            sgr_pixels,
            last_geometry,
        ) {
            return;
        }
        if (framer.has_pending_input() || !pending_palette.is_empty())
            && stdin_read_ready(
                &reader,
                framer.idle_flush_timeout_ms(host_mouse_capture_active.load(Ordering::Acquire)),
            ) == Some(false)
        {
            let had_pending = framer.has_pending_input();
            let chunks = framer.flush_timeout();
            let held_escape = had_pending && chunks.is_empty();
            if !send_unix_input_chunks(
                chunks,
                &event_tx,
                &mut pending_palette,
                sgr_pixels,
                last_geometry,
            ) || !flush_unix_palette_input(&event_tx, &mut pending_palette)
            {
                return;
            }
            if held_escape
                && stdin_read_ready(&reader, framer.held_input_flush_timeout_ms()) == Some(false)
                && !send_unix_input_chunks(
                    framer.flush_timeout(),
                    &event_tx,
                    &mut pending_palette,
                    sgr_pixels,
                    last_geometry,
                )
            {
                return;
            }
        }
        pending_mode = framer.has_pending_input().then_some(sgr_pixels);
    }

    while !should_quit.load(Ordering::Acquire) {
        if direct_filter.has_pending()
            && stdin_read_ready(&reader, crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS)
                == Some(false)
        {
            let released = direct_response
                .lock()
                .ok()
                .and_then(|mut matcher| direct_filter.flush_if_inactive(&mut matcher));
            if let Some(data) = released {
                if event_tx
                    .blocking_send(ClientLoopEvent::StdinInput(data))
                    .is_err()
                {
                    return;
                }
            }
            continue;
        }
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => {
                // A redraw can issue queries while this thread is blocked in read().
                // Arm the split-reply guard before framing the returned bytes.
                for _ in 0..host_theme_query_pending.swap(0, Ordering::AcqRel) {
                    framer.host_color_query_sent();
                }
                let sgr_pixels = *pending_mode
                    .get_or_insert_with(|| host_sgr_pixels_active.load(Ordering::Acquire));
                if sgr_pixels {
                    last_geometry = retain_geometry(
                        last_geometry,
                        crate::input::mouse::HostGeometry::current(),
                    );
                }
                let filtered = filter_direct_input(
                    &scratch[..n],
                    &mut direct_filter,
                    &direct_response,
                    &direct_response_active,
                );
                let chunks = if let Some((raw_chunks, responses)) = filtered {
                    for response in responses {
                        if event_tx
                            .blocking_send(ClientLoopEvent::DirectGraphicsResponse(response))
                            .is_err()
                        {
                            return;
                        }
                    }
                    raw_chunks
                        .into_iter()
                        .flat_map(|chunk| framer.push(&chunk))
                        .collect()
                } else {
                    framer.push(&scratch[..n])
                };
                if !framer.has_pending_input() {
                    pending_mode = None;
                }
                if !send_unix_input_chunks(
                    chunks,
                    &event_tx,
                    &mut pending_palette,
                    sgr_pixels,
                    last_geometry,
                ) {
                    return;
                }

                let timeout_ms =
                    framer.idle_flush_timeout_ms(host_mouse_capture_active.load(Ordering::Acquire));
                if stdin_read_ready(&reader, timeout_ms) == Some(false) {
                    let had_pending = framer.has_pending_input();
                    let chunks = framer.flush_timeout();
                    let held_escape = had_pending && chunks.is_empty();
                    let sgr_pixels = pending_mode
                        .unwrap_or_else(|| host_sgr_pixels_active.load(Ordering::Acquire));
                    if !framer.has_pending_input() {
                        pending_mode = None;
                    }
                    if !send_unix_input_chunks(
                        chunks,
                        &event_tx,
                        &mut pending_palette,
                        sgr_pixels,
                        last_geometry,
                    ) || !flush_unix_palette_input(&event_tx, &mut pending_palette)
                    {
                        return;
                    }
                    if held_escape
                        && stdin_read_ready(&reader, framer.held_input_flush_timeout_ms())
                            == Some(false)
                    {
                        let chunks = framer.flush_timeout();
                        if !framer.has_pending_input() {
                            pending_mode = None;
                        }
                        if !send_unix_input_chunks(
                            chunks,
                            &event_tx,
                            &mut pending_palette,
                            sgr_pixels,
                            last_geometry,
                        ) {
                            return;
                        }
                    }
                }
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }
}

#[cfg(unix)]
fn filter_direct_input(
    bytes: &[u8],
    filter: &mut super::direct_graphics::InputFilter,
    response: &std::sync::Mutex<super::direct_graphics::ResponseMatcher>,
    active: &AtomicBool,
) -> Option<(Vec<Vec<u8>>, Vec<super::direct_graphics::Response>)> {
    if !active.load(Ordering::Acquire) && !filter.has_pending() {
        return None;
    }
    Some(
        response
            .lock()
            .map(|mut matcher| filter.push(bytes, &mut matcher))
            .unwrap_or_else(|_| (vec![bytes.to_vec()], Vec::new())),
    )
}

#[cfg(unix)]
fn send_unix_input_chunks(
    chunks: Vec<Vec<u8>>,
    event_tx: &mpsc::Sender<ClientLoopEvent>,
    pending_palette: &mut Vec<Vec<u8>>,
    sgr_pixels: bool,
    geometry: Option<crate::input::mouse::HostGeometry>,
) -> bool {
    for data in chunks {
        let palette_response = std::str::from_utf8(&data)
            .ok()
            .and_then(crate::terminal_theme::parse_palette_color_response)
            .is_some();
        if palette_response {
            pending_palette.push(data);
            if pending_palette.len() == 256 && !flush_unix_palette_input(event_tx, pending_palette)
            {
                return false;
            }
            continue;
        }
        let default_color_response = std::str::from_utf8(&data)
            .ok()
            .and_then(crate::terminal_theme::parse_default_color_response)
            .is_some();
        if !default_color_response && !flush_unix_palette_input(event_tx, pending_palette) {
            return false;
        }
        let Some(event) = classify_unix_input(data, sgr_pixels, geometry) else {
            continue;
        };
        if event_tx.blocking_send(event).is_err() {
            return false;
        }
    }
    true
}

#[cfg(unix)]
fn retain_geometry(
    last: Option<crate::input::mouse::HostGeometry>,
    observed: Option<crate::input::mouse::HostGeometry>,
) -> Option<crate::input::mouse::HostGeometry> {
    observed.or(last)
}

#[cfg(unix)]
fn classify_unix_input(
    data: Vec<u8>,
    sgr_pixels: bool,
    geometry: Option<crate::input::mouse::HostGeometry>,
) -> Option<ClientLoopEvent> {
    if sgr_pixels && crate::raw_input::parse_sgr_mouse_report(&data).is_some() {
        return geometry.map(|geometry| ClientLoopEvent::PixelMouse(data, geometry));
    }
    Some(ClientLoopEvent::StdinInput(data))
}

#[cfg(unix)]
fn flush_unix_palette_input(
    event_tx: &mpsc::Sender<ClientLoopEvent>,
    pending_palette: &mut Vec<Vec<u8>>,
) -> bool {
    if pending_palette.is_empty() {
        return true;
    }
    let data = std::mem::take(pending_palette).concat();
    event_tx
        .blocking_send(ClientLoopEvent::StdinInput(data))
        .is_ok()
}

#[cfg(windows)]
fn windows_stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
) {
    match windows_vti::console_input_handle() {
        Ok(handle) => {
            windows_vti::trace_input_transport("reader=windows-console");
            windows_vti::raw_console_reader_loop(
                handle,
                event_tx,
                should_quit,
                host_color_query_sent,
            );
        }
        Err(err) => {
            tracing::error!(%err, "no Windows console input available; keyboard input is disabled");
        }
    }
}

#[cfg(any(windows, test))]
fn windows_client_input_event_from_raw(
    event: crate::raw_input::RawInputEvent,
) -> Option<crate::protocol::ClientInputEvent> {
    match event {
        crate::raw_input::RawInputEvent::Text(text) => Some(
            crate::protocol::ClientInputEvent::TextCommit(text.into_string()),
        ),
        crate::raw_input::RawInputEvent::Key(key) => {
            let code = crate::protocol::ClientKeyCode::from_crossterm(key.code)?;
            let modifiers = key.modifiers.bits();
            let kind = crate::protocol::ClientKeyKind::from_crossterm(key.kind);
            let source = if let Some(bytes) = key.vt_bytes() {
                crate::protocol::ClientKeySource::Vt {
                    bytes: bytes.to_vec(),
                }
            } else if let Some(record) = key.windows_record() {
                crate::protocol::ClientKeySource::WindowsConsole { record }
            } else {
                crate::protocol::ClientKeySource::Synthesized
            };
            Some(crate::protocol::ClientInputEvent::Key {
                code,
                modifiers,
                kind,
                repeat_count: key.repeat_count,
                generated_text: key.generated_text.clone(),
                source,
            })
        }
        crate::raw_input::RawInputEvent::Mouse(mouse) => {
            Some(crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::from_crossterm(mouse.kind)?,
                column: mouse.column,
                row: mouse.row,
                modifiers: mouse.modifiers.bits(),
            })
        }
        crate::raw_input::RawInputEvent::Paste(text) => {
            Some(crate::protocol::ClientInputEvent::Paste { text })
        }
        crate::raw_input::RawInputEvent::OuterFocusGained => {
            Some(crate::protocol::ClientInputEvent::FocusGained)
        }
        crate::raw_input::RawInputEvent::OuterFocusLost => {
            Some(crate::protocol::ClientInputEvent::FocusLost)
        }
        crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
            Some(crate::protocol::ClientInputEvent::HostDefaultColor {
                kind: match kind {
                    crate::terminal_theme::DefaultColorKind::Foreground => {
                        crate::protocol::ClientHostDefaultColorKind::Foreground
                    }
                    crate::terminal_theme::DefaultColorKind::Background => {
                        crate::protocol::ClientHostDefaultColorKind::Background
                    }
                },
                color: color.into(),
            })
        }
        crate::raw_input::RawInputEvent::HostPaletteColors { .. }
        | crate::raw_input::RawInputEvent::HostColorSchemeChanged(_)
        | crate::raw_input::RawInputEvent::HostCellSizeReport { .. }
        | crate::raw_input::RawInputEvent::Unsupported => None,
    }
}

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(reader: &R, timeout_ms: i32) -> Option<bool> {
    poll_read_ready(reader.as_raw_fd(), timeout_ms)
}

#[cfg(unix)]
fn poll_read_ready(fd: i32, timeout_ms: i32) -> Option<bool> {
    crate::platform::poll_fd_readable(fd, timeout_ms).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    // The stdin reader thread is hard to unit test since it reads from actual stdin.
    // Integration tests will verify the full client→server input flow.
    // Here we test the event type construction.

    use super::*;

    #[cfg(unix)]
    #[test]
    fn stdin_input_event_carries_raw_bytes() {
        let data = vec![0x1b, b'[', b'A']; // Up arrow escape sequence
        let event = ClientLoopEvent::StdinInput(data.clone());
        match event {
            ClientLoopEvent::StdinInput(d) => assert_eq!(d, data),
            _ => panic!("expected StdinInput event"),
        }
    }

    #[test]
    fn inactive_direct_input_bypasses_filter() {
        let response =
            std::sync::Mutex::new(super::super::direct_graphics::ResponseMatcher::default());
        let active = response.lock().unwrap().active_handle();
        let mut filter = super::super::direct_graphics::InputFilter::default();
        assert!(filter_direct_input(b"typed", &mut filter, &response, &active).is_none());
        assert!(!filter.has_pending());
    }

    #[test]
    fn pixel_mouse_classification_is_narrow_and_uses_read_geometry() {
        let geometry = crate::input::mouse::HostGeometry::new(80, 24, 800, 480).unwrap();
        let report = b"\x1b[<35;321;241M".to_vec();
        let Some(ClientLoopEvent::PixelMouse(data, captured)) =
            classify_unix_input(report.clone(), true, Some(geometry))
        else {
            panic!("expected dedicated pixel mouse event");
        };
        assert_eq!(data, report);
        assert_eq!(captured, geometry);
        assert!(classify_unix_input(report, true, None).is_none());

        for raw in [
            b"key".as_slice(),
            b"\x1b[200~paste\x1b[201~".as_slice(),
            b"\x1b_Gi=7;unrelated\x1b\\".as_slice(),
            b"\x1b[<35;2;3Mtail".as_slice(),
        ] {
            let Some(ClientLoopEvent::StdinInput(data)) =
                classify_unix_input(raw.to_vec(), true, Some(geometry))
            else {
                panic!("unrelated input must remain raw");
            };
            assert_eq!(data, raw);
        }
    }

    #[test]
    fn transient_geometry_failure_keeps_last_real_value() {
        let geometry = crate::input::mouse::HostGeometry::new(80, 24, 800, 480).unwrap();
        assert_eq!(retain_geometry(Some(geometry), None), Some(geometry));
    }

    #[test]
    fn palette_replies_are_forwarded_as_one_input_batch() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut pending = Vec::new();
        assert!(send_unix_input_chunks(
            vec![
                b"\x1b]4;0;rgb:1111/2222/3333\x1b\\".to_vec(),
                b"\x1b]4;1;rgb:4444/5555/6666\x1b\\".to_vec(),
            ],
            &tx,
            &mut pending,
            false,
            None,
        ));
        assert!(rx.try_recv().is_err());

        assert!(flush_unix_palette_input(&tx, &mut pending));
        let ClientLoopEvent::StdinInput(data) = rx.try_recv().unwrap() else {
            panic!("expected palette input batch");
        };
        assert_eq!(
            data.windows(4)
                .filter(|window| *window == b"\x1b]4;")
                .count(),
            2
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn raw_input_idle_flush_timeout_keeps_escape_responsive() {
        let timeout_ms = std::hint::black_box(crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS);
        assert!(timeout_ms <= 20);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn windows_repeated_escape_keeps_second_escape_pending() {
        let mut framer = crate::raw_input::RawInputFramer::for_host_input();

        let events = framer.push(b"\x1b\x1b");

        assert_eq!(events.len(), 1);
        // The second escape is still pending until the idle flush.
        assert_eq!(framer.flush_timeout().len(), 1);
    }

    /// Applies the Unix reader's idle waits for a gap between two host writes
    /// without sleeping, using its production timeout selector.
    fn reader_chunks_across_gap(
        framer: &mut crate::raw_input::RawInputByteFramer,
        mouse_capture: bool,
        gap: std::time::Duration,
        next: &[u8],
    ) -> Vec<Vec<u8>> {
        let first_wait =
            std::time::Duration::from_millis(framer.idle_flush_timeout_ms(mouse_capture) as u64);
        let mut chunks = Vec::new();
        if gap >= first_wait {
            chunks.extend(framer.flush_timeout());
            if chunks.is_empty()
                && gap
                    >= first_wait
                        + std::time::Duration::from_millis(
                            framer.held_input_flush_timeout_ms() as u64
                        )
            {
                chunks.extend(framer.flush_timeout());
            }
        }
        chunks.extend(framer.push(next));
        chunks
    }

    fn confirmed_disambiguation_framer() -> crate::raw_input::RawInputByteFramer {
        let mut framer = crate::raw_input::RawInputByteFramer::for_host_input();
        framer.set_host_escape_disambiguation_active(true);
        framer
    }

    #[test]
    fn confirmed_disambiguation_keeps_mouse_report_split_by_delayed_tail() {
        // #3480: the tail of a click arrived 350 ms after its ESC.
        let gap = std::time::Duration::from_millis(350);
        for (prefix, tail) in [
            (b"\x1b".as_slice(), b"[<0;5;5M".as_slice()),
            (b"\x1b[", b"<0;5;5M"),
            (b"\x1b[<0;", b"5;5M"),
        ] {
            let mut framer = confirmed_disambiguation_framer();
            assert!(framer.push(prefix).is_empty());

            assert_eq!(
                reader_chunks_across_gap(&mut framer, true, gap, tail),
                vec![b"\x1b[<0;5;5M".to_vec()],
                "prefix {prefix:?} must rejoin its mouse tail"
            );
        }
    }

    #[test]
    fn confirmed_disambiguation_keeps_escape_prefixed_bindings_intact() {
        // Ghostty's macOS Alt+Left/Right and Alacritty Shift+Enter bindings (#4751).
        // iTerm2's Natural Text Editing preset also sends ESC DEL for Option+Backspace.
        for binding in [b"\x1bb".as_slice(), b"\x1bf", b"\x1b\r", b"\x1b\x7f"] {
            let mut framer = confirmed_disambiguation_framer();

            assert_eq!(framer.push(binding), vec![binding.to_vec()]);
        }
    }

    #[test]
    fn confirmed_disambiguation_releases_escape_prefixed_bindings_after_bounded_wait() {
        // Terminal text bindings can send ESC, Alt+[ or Alt+O alone (#4751).
        let gap = std::time::Duration::from_secs(1);
        for binding in [b"\x1b".as_slice(), b"\x1b[", b"\x1bO"] {
            let mut framer = confirmed_disambiguation_framer();
            assert!(framer.push(binding).is_empty());

            assert_eq!(
                reader_chunks_across_gap(&mut framer, true, gap, b"x"),
                vec![binding.to_vec(), b"x".to_vec()],
                "binding {binding:?} must not be held or dropped"
            );
        }
    }

    #[test]
    fn confirmed_disambiguation_separates_escape_prefixed_binding_from_fast_next_key() {
        // Typing right after an Alt+O / Alt+[ / ESC text binding must not glue
        // the next key onto it while Herdr is watching for a delayed mouse tail.
        for (binding, next, gap_ms) in [
            (b"\x1bO".as_slice(), b"q".as_slice(), 80),
            (b"\x1b[", b"A", 80),
            (b"\x1b", b"x", 200),
        ] {
            let mut framer = confirmed_disambiguation_framer();
            assert!(framer.push(binding).is_empty());

            assert_eq!(
                reader_chunks_across_gap(
                    &mut framer,
                    true,
                    std::time::Duration::from_millis(gap_ms),
                    next
                ),
                vec![binding.to_vec(), next.to_vec()],
                "binding {binding:?} followed by {next:?} after {gap_ms} ms"
            );
        }
    }

    #[test]
    fn confirmed_disambiguation_releases_binding_while_host_reply_is_pending() {
        for (binding, next) in [(b"\x1b[".as_slice(), b"A".as_slice()), (b"\x1b", b"x")] {
            let mut framer = confirmed_disambiguation_framer();
            framer.host_cell_size_query_sent();
            framer.host_color_query_sent();
            assert!(framer.push(binding).is_empty());

            assert_eq!(
                reader_chunks_across_gap(
                    &mut framer,
                    true,
                    std::time::Duration::from_secs(1),
                    next
                ),
                vec![binding.to_vec(), next.to_vec()],
                "binding {binding:?} must not stay held behind a pending host reply"
            );
        }
    }

    #[test]
    fn confirmed_disambiguation_keeps_slow_host_reply_and_paste_whole() {
        let paste = b"200~hello\n\x1b[201~";
        for (tail, gap_ms) in [(b"6;21;10t".as_slice(), 55), (paste.as_slice(), 80)] {
            let mut framer = confirmed_disambiguation_framer();
            framer.host_cell_size_query_sent();
            assert!(framer.push(b"\x1b[").is_empty());

            let mut whole = b"\x1b[".to_vec();
            whole.extend_from_slice(tail);
            assert_eq!(
                reader_chunks_across_gap(
                    &mut framer,
                    true,
                    std::time::Duration::from_millis(gap_ms),
                    tail
                ),
                vec![whole],
                "tail {tail:?} after {gap_ms} ms must stay attached to ESC["
            );
        }
    }

    #[test]
    fn confirmed_disambiguation_keeps_osc_reply_split_after_escape_whole() {
        let mut framer = confirmed_disambiguation_framer();
        framer.host_color_query_sent();
        assert!(framer.push(b"\x1b").is_empty());

        let tail = b"]11;rgb:1111/2222/3333\x07";
        let mut whole = b"\x1b".to_vec();
        whole.extend_from_slice(tail);
        assert_eq!(
            reader_chunks_across_gap(
                &mut framer,
                true,
                std::time::Duration::from_millis(155),
                tail
            ),
            vec![whole]
        );
    }

    #[test]
    fn confirmed_disambiguation_mouse_wait_does_not_consume_later_reply_hold() {
        let mut framer = confirmed_disambiguation_framer();
        framer.enable_host_appearance_query_on_focus();
        assert_eq!(framer.push(b"\x1b[I"), vec![b"\x1b[I".to_vec()]);
        assert!(framer.push(b"\x1b[<0;").is_empty());

        assert!(reader_chunks_across_gap(
            &mut framer,
            true,
            std::time::Duration::from_millis(650),
            b"\x1b[?997;"
        )
        .is_empty());
        assert_eq!(
            reader_chunks_across_gap(
                &mut framer,
                true,
                std::time::Duration::from_millis(15),
                b"2n"
            ),
            vec![b"\x1b[?997;2n".to_vec()]
        );
    }

    #[test]
    fn confirmed_disambiguation_keeps_kitty_escape_immediate() {
        let mut framer = confirmed_disambiguation_framer();

        assert_eq!(framer.push(b"\x1b[27u"), vec![b"\x1b[27u".to_vec()]);
    }

    #[test]
    fn confirmed_disambiguation_joins_escape_binding_split_before_wait_ends() {
        let mut framer = confirmed_disambiguation_framer();
        assert!(framer.push(b"\x1b").is_empty());

        assert_eq!(
            reader_chunks_across_gap(
                &mut framer,
                true,
                std::time::Duration::from_millis(40),
                b"\r"
            ),
            vec![b"\x1b\r".to_vec()]
        );
    }

    #[test]
    fn legacy_alt_bracket_is_not_glued_to_following_key() {
        let mut framer = crate::raw_input::RawInputByteFramer::for_host_input();
        assert!(framer.push(b"\x1b[").is_empty());

        assert_eq!(
            reader_chunks_across_gap(
                &mut framer,
                true,
                std::time::Duration::from_millis(80),
                b"a"
            ),
            vec![b"\x1b[".to_vec(), b"a".to_vec()]
        );
    }

    #[test]
    fn captured_mouse_report_split_after_csi_survives_idle_gap() {
        let mut framer = crate::raw_input::RawInputByteFramer::for_host_input();
        framer.enable_host_appearance_query_on_focus();
        assert_eq!(framer.push(b"\x1b[I"), vec![b"\x1b[I".to_vec()]);
        assert!(framer.push(b"\x1b[").is_empty());

        // #4630 input.bin offsets 10465/10467: ESC[ and its continuation
        // arrived 32.937 ms apart.
        let chunks = reader_chunks_across_gap(
            &mut framer,
            true,
            std::time::Duration::from_micros(32_937),
            b"<35;64;37M\x1b[<35;65;36M\x1b[<35;64;36M",
        );
        assert_eq!(
            chunks,
            vec![
                b"\x1b[<35;64;37M".to_vec(),
                b"\x1b[<35;65;36M".to_vec(),
                b"\x1b[<35;64;36M".to_vec(),
            ],
            "captured mouse reports must remain whole, not become key fragments"
        );
        assert!(!framer.has_pending_input());
        assert_eq!(framer.push(b"x"), vec![b"x".to_vec()]);
    }

    #[test]
    fn only_ambiguous_escape_prefixes_get_the_short_keyboard_window() {
        let mut escape = crate::raw_input::RawInputByteFramer::default();
        assert!(escape.push(b"\x1b").is_empty());
        let mut csi = crate::raw_input::RawInputByteFramer::default();
        assert!(csi.push(b"\x1b[").is_empty());
        let mut sgr_mouse = crate::raw_input::RawInputByteFramer::default();
        assert!(sgr_mouse.push(b"\x1b[<3").is_empty());
        let mut default_mouse = crate::raw_input::RawInputByteFramer::default();
        assert!(default_mouse.push(b"\x1b[MC").is_empty());
        let mut unrelated = crate::raw_input::RawInputByteFramer::default();
        assert!(unrelated.push(b"\x1b[49:33;2:").is_empty());

        // A lone ESC or ESC[ may still be a key (Escape, Alt+[).
        for framer in [&escape, &csi] {
            assert_eq!(
                framer.idle_flush_timeout_ms(false),
                crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS
            );
        }
        assert_eq!(
            escape.idle_flush_timeout_ms(true),
            crate::raw_input::MOUSE_ACTIVE_ESCAPE_SEQUENCE_FLUSH_TIMEOUT_MS
        );
        assert_eq!(
            csi.idle_flush_timeout_ms(true),
            crate::raw_input::MOUSE_ACTIVE_CSI_INTRODUCER_FLUSH_TIMEOUT_MS
        );
        // Anything further inside a sequence cannot be a key: wait for the rest.
        for framer in [&sgr_mouse, &default_mouse, &unrelated] {
            for mouse_capture in [false, true] {
                assert_eq!(
                    framer.idle_flush_timeout_ms(mouse_capture),
                    crate::raw_input::INCOMPLETE_SEQUENCE_FLUSH_TIMEOUT_MS
                );
            }
        }

        let mouse_timeout_ms =
            std::hint::black_box(crate::raw_input::MOUSE_ACTIVE_ESCAPE_SEQUENCE_FLUSH_TIMEOUT_MS);
        assert!(mouse_timeout_ms > 100);
    }
}

#[cfg(test)]
mod windows_tests {
    use super::*;

    #[test]
    fn windows_pending_escape_sequence_converts_to_semantic_arrow() {
        let mut framer = crate::raw_input::RawInputFramer::default();
        assert!(framer.push(b"\x1b").is_empty());
        assert!(framer.push(b"[").is_empty());
        let events = framer.push(b"A");
        assert_eq!(events.len(), 1);

        let event = windows_client_input_event_from_raw(events.into_iter().next().unwrap())
            .expect("raw arrow converts");
        assert_eq!(
            event,
            crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Up,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Vt {
                    bytes: b"\x1b[A".to_vec()
                },
            }
        );
    }

    #[test]
    fn windows_bare_escape_flushes_to_semantic_escape() {
        let mut framer = crate::raw_input::RawInputFramer::default();
        assert!(framer.push(b"\x1b").is_empty());
        let events = framer.flush_timeout();
        assert_eq!(events.len(), 1);

        let event = windows_client_input_event_from_raw(events.into_iter().next().unwrap())
            .expect("raw escape converts");
        assert_eq!(
            event,
            crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Vt { bytes: vec![0x1b] },
            }
        );
    }
}
