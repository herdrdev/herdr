//! Blocking client socket transport for the headless server.
//!
//! This module owns the thin-client handshake, read loop, and writer loop.
//! It converts socket I/O into [`ServerEvent`] values consumed by
//! `HeadlessServer`.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::ipc::LocalStream;
use crate::protocol::endpoint::{
    EndpointClientHello, EndpointServerWelcome, ENDPOINT_HELLO_KIND, ENDPOINT_PROTOCOL_GENERATION,
    ENDPOINT_WELCOME_KIND,
};
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, ClientMessage, ClientPaneInputEvent,
    RenderEncoding, ServerMessage, MAX_CLIPBOARD_IMAGE_PAYLOAD, MAX_FRAME_SIZE,
    MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};

/// Minimum accepted attached client size.
///
/// Narrow observers must be allowed to drive narrow renders, otherwise the
/// server wraps pane content against a wider width and the client sees the
/// right edge clipped.
const MIN_CLIENT_COLS: u16 = 1;
const MIN_CLIENT_ROWS: u16 = 1;

/// How long to wait for a client handshake before closing the connection.
/// Set to 4 seconds (rather than 5) to guarantee the connection is closed
/// within the 5-second deadline, even with OS timer slack, thread scheduling,
/// and cleanup overhead.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);

/// Maximum input payload size (bytes) for a single `ClientMessage::Input`.
const MAX_INPUT_PAYLOAD: usize = 1024 * 1024; // 1 MB
const MAX_CLIENT_SHELL_DIMENSION: u16 = 4096;
const MAX_CLIENT_SHELL_CELLS: u32 = 1_000_000;
const MAX_CLIENT_CELL_SIZE_PX: u32 = 4096;

fn client_shell_geometry_error(
    surface_size: crate::protocol::ClientSurfaceSize,
    cell_width_px: u32,
    cell_height_px: u32,
) -> Option<&'static str> {
    if surface_size.cols == 0 || surface_size.rows == 0 {
        return Some("client shell requires a non-empty pane surface");
    }
    if surface_size.cols > MAX_CLIENT_SHELL_DIMENSION
        || surface_size.rows > MAX_CLIENT_SHELL_DIMENSION
        || u32::from(surface_size.cols) * u32::from(surface_size.rows) > MAX_CLIENT_SHELL_CELLS
    {
        return Some("client shell pane surface exceeds the safe geometry limit");
    }
    if cell_width_px > MAX_CLIENT_CELL_SIZE_PX || cell_height_px > MAX_CLIENT_CELL_SIZE_PX {
        return Some("client shell cell pixel size exceeds the safe geometry limit");
    }
    None
}

#[derive(serde::Deserialize)]
struct EndpointRequestHead {
    id: String,
    method: String,
}

enum DecodedEndpointRequest {
    Dispatch(Box<crate::api::schema::Request>),
    Error {
        request_id: String,
        code: &'static str,
        message: String,
    },
}

fn write_endpoint_rejection(stream: &mut LocalStream, code: &str, message: impl Into<String>) {
    let welcome = EndpointServerWelcome::incompatible(code, message);
    let response = ServerMessage::EndpointControl {
        kind: ENDPOINT_WELCOME_KIND.into(),
        data: serde_json::to_string(&welcome).unwrap_or_else(|_| "{}".into()),
    };
    let _ = protocol::write_message(stream, &response);
}

fn decode_endpoint_request(request: &str) -> serde_json::Result<DecodedEndpointRequest> {
    let head = serde_json::from_str::<EndpointRequestHead>(request)?;
    if !crate::server::client_commands::supports_client_shell_method_name(&head.method) {
        return Ok(DecodedEndpointRequest::Error {
            request_id: head.id,
            code: "unsupported_method",
            message: format!("method {:?} is not available on this machine", head.method),
        });
    }
    Ok(
        match serde_json::from_str::<crate::api::schema::Request>(request) {
            Ok(request) => DecodedEndpointRequest::Dispatch(Box::new(request)),
            Err(error) => DecodedEndpointRequest::Error {
                request_id: head.id,
                code: "invalid_request",
                message: format!("invalid endpoint request: {error}"),
            },
        },
    )
}
/// Maximum structured input events accepted in one client message.
const MAX_INPUT_EVENT_BATCH: usize = 4096;

/// Channels owned by the server side of a client writer thread.
#[derive(Clone, Debug)]
pub(crate) struct ClientWriter {
    /// Reliable control messages such as shutdown, notifications, and clipboard writes.
    pub(crate) control: ClientControlWriter,
    /// Droppable render messages. Capacity is one so slow clients cannot build lag.
    pub(crate) render: ClientRenderWriter,
}

impl ClientWriter {
    #[cfg(test)]
    pub(crate) fn test_fill_render(&self, data: Vec<u8>) {
        self.render.try_send(data).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn test_close(&self) {
        self.render.queue.close_writer();
    }

    #[cfg(test)]
    pub(crate) fn test_channel(
        control: std::sync::mpsc::Sender<Vec<u8>>,
        render: std::sync::mpsc::SyncSender<Vec<u8>>,
    ) -> Self {
        let queue = ClientWriterQueue::new();
        let drain = queue.clone();
        let control_writer = ClientControlWriter::queue(queue.clone());
        let mut render_writer = ClientRenderWriter::queue(queue);
        render_writer.test_render = Some(render.clone());
        let writer = Self {
            control: control_writer,
            render: render_writer,
        };
        std::thread::spawn(move || {
            while let Some(item) = drain.recv() {
                let sent = match item {
                    ClientWriteItem::Control(data) => control.send(data).is_ok(),
                    ClientWriteItem::Render(data) => render.send(data).is_ok(),
                };
                if !sent {
                    break;
                }
            }
            drain.close_writer();
        });
        writer
    }
}

#[derive(Debug)]
pub(crate) struct ClientControlWriter {
    queue: Arc<ClientWriterQueue>,
    #[cfg(test)]
    test_render: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
}

#[derive(Debug)]
pub(crate) struct ClientRenderWriter {
    queue: Arc<ClientWriterQueue>,
    #[cfg(test)]
    test_render: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
}

macro_rules! writer_handle {
    ($type:ty) => {
        impl Clone for $type {
            fn clone(&self) -> Self {
                self.queue.add_sender();
                Self {
                    queue: self.queue.clone(),
                    #[cfg(test)]
                    test_render: self.test_render.clone(),
                }
            }
        }
        impl Drop for $type {
            fn drop(&mut self) {
                self.queue.remove_sender();
            }
        }
    };
}
writer_handle!(ClientControlWriter);
writer_handle!(ClientRenderWriter);

impl ClientControlWriter {
    fn queue(queue: Arc<ClientWriterQueue>) -> Self {
        queue.add_sender();
        Self {
            queue,
            #[cfg(test)]
            test_render: None,
        }
    }

    pub(crate) fn send(&self, data: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        self.queue.send_control(data)
    }
}

impl ClientRenderWriter {
    fn queue(queue: Arc<ClientWriterQueue>) -> Self {
        queue.add_sender();
        Self {
            queue,
            #[cfg(test)]
            test_render: None,
        }
    }

    pub(crate) fn try_send(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        #[cfg(test)]
        if let Some(sender) = &self.test_render {
            return sender.try_send(data);
        }
        self.queue.try_send_render(data)
    }

    pub(crate) fn send_ordered(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        self.queue.send_ordered(data)
    }
}

#[derive(Debug)]
struct ClientWriterQueue {
    state: Mutex<ClientWriterQueueState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct ClientWriterQueueState {
    control: VecDeque<Vec<u8>>,
    ordered: VecDeque<Vec<u8>>,
    render: Option<Vec<u8>>,
    senders: usize,
    writer_alive: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ClientWriteItem {
    Control(Vec<u8>),
    Render(Vec<u8>),
}

impl ClientWriterQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ClientWriterQueueState {
                writer_alive: true,
                ..ClientWriterQueueState::default()
            }),
            ready: Condvar::new(),
        })
    }

    fn add_sender(&self) {
        let mut state = self.lock_state();
        state.senders = state.senders.saturating_add(1);
    }

    fn remove_sender(&self) {
        let mut state = self.lock_state();
        state.senders = state.senders.saturating_sub(1);
        self.ready.notify_one();
    }

    fn send_control(&self, data: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        let mut state = self.lock_state();
        if !state.writer_alive {
            return Err(SendError(data));
        }
        state.control.push_back(data);
        self.ready.notify_one();
        Ok(())
    }

    fn try_send_render(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        let mut state = self.lock_state();
        if !state.writer_alive {
            return Err(TrySendError::Disconnected(data));
        }
        if state.render.is_some() {
            return Err(TrySendError::Full(data));
        }
        state.render = Some(data);
        self.ready.notify_one();
        Ok(())
    }

    fn send_ordered(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        let mut state = self.lock_state();
        if !state.writer_alive {
            return Err(TrySendError::Disconnected(data));
        }
        if !state.ordered.is_empty() {
            return Err(TrySendError::Full(data));
        }
        if let Some(older) = state.render.take() {
            state.ordered.push_back(older);
        }
        state.ordered.push_back(data);
        self.ready.notify_one();
        Ok(())
    }

    fn recv(&self) -> Option<ClientWriteItem> {
        let mut state = self.lock_state();
        loop {
            if let Some(data) = state.control.pop_front() {
                return Some(ClientWriteItem::Control(data));
            }
            if let Some(data) = state.ordered.pop_front() {
                self.ready.notify_one();
                return Some(ClientWriteItem::Render(data));
            }
            if let Some(data) = state.render.take() {
                return Some(ClientWriteItem::Render(data));
            }
            if state.senders == 0 {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn close_writer(&self) {
        let mut state = self.lock_state();
        state.writer_alive = false;
        state.render = None;
        state.ordered.clear();
        self.ready.notify_all();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ClientWriterQueueState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Internal event sent from client transport threads to the main event loop.
#[derive(Debug)]
pub(crate) enum ServerEvent {
    /// A new client completed the handshake.
    ClientConnected {
        client_id: u64,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        pixel_mouse: bool,
        writer: ClientWriter,
    },
    /// A client-owned shell completed its dedicated handshake.
    ClientShellConnected {
        client_id: u64,
        surface_cols: u16,
        surface_rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        pixel_mouse: bool,
        direct_graphics: bool,
        endpoint_keybindings: bool,
        mouse_capture: bool,
        writer: ClientWriter,
    },
    /// A client sent an input message.
    ClientInput { client_id: u64, data: Vec<u8> },
    /// A client reported the one armed Kitty regular-file response.
    GraphicsTransmissionResult {
        client_id: u64,
        transfer_id: u64,
        image_id: u32,
        success: bool,
    },
    GraphicsTransmissionStarted {
        client_id: u64,
        transfer_id: u64,
        image_id: u32,
    },
    /// A fully decoded interactive paste exceeded the text-input limit.
    ClientPasteRejected {
        client_id: u64,
        size: usize,
        max: usize,
    },
    /// A client sent local clipboard image bytes to paste into a remote pane.
    ClientClipboardImage {
        client_id: u64,
        target: crate::protocol::ClientClipboardImageTarget,
        extension: String,
        data: Vec<u8>,
    },
    /// A client requested direct attach to one terminal.
    ClientAttachTerminal {
        client_id: u64,
        terminal_id: String,
        takeover: bool,
    },
    /// A client requested read-only observation of one terminal.
    ClientObserveTerminal { client_id: u64, target: String },
    /// A client requested writable control of one terminal.
    ClientControlTerminal {
        client_id: u64,
        target: String,
        takeover: bool,
    },
    /// A direct terminal attach client requested scrollback movement.
    ClientAttachScroll {
        client_id: u64,
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    },
    /// A direct terminal attach client delivered one structured mouse event.
    ClientAttachMouse {
        client_id: u64,
        kind: crate::protocol::ClientMouseKind,
        position: crate::protocol::ClientMousePosition,
        geometry: Option<crate::protocol::ClientMouseGeometry>,
        modifiers: u8,
        lines: u16,
    },
    /// A client sent a resize message.
    ClientResize {
        client_id: u64,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        pixel_mouse: bool,
    },
    /// A client-owned shell recomputed its pane viewport.
    ClientShellResize {
        client_id: u64,
        surface_cols: u16,
        surface_rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        pixel_mouse: bool,
    },
    /// A client-owned shell delivered semantic input to one stable pane target.
    ClientShellPaneInput {
        client_id: u64,
        pane_id: String,
        events: Vec<ClientPaneInputEvent>,
    },
    /// A client-owned shell delivered semantic input to its active popup terminal.
    ClientShellPopupInput {
        client_id: u64,
        terminal_id: String,
        events: Vec<ClientPaneInputEvent>,
    },
    /// A client-owned shell published one host terminal theme observation.
    ClientShellHostTheme {
        client_id: u64,
        update: crate::protocol::ClientHostThemeUpdate,
    },
    /// A client-owned shell reported whether its outer terminal has focus.
    ClientShellFocus { client_id: u64, focused: bool },
    /// A client-owned shell updated its local mouse-capture preference.
    ClientShellMouseCapture { client_id: u64, enabled: bool },
    /// A client-owned shell invoked one endpoint operation through this connection.
    ClientShellEndpointRequest {
        client_id: u64,
        boot_id: String,
        request: Box<crate::api::schema::Request>,
    },
    /// A well-framed endpoint request could not be dispatched by this server.
    ClientShellEndpointRequestError {
        client_id: u64,
        boot_id: String,
        request_id: String,
        code: &'static str,
        message: String,
    },
    /// One chunk of a deferred endpoint operation's final response is ready.
    ClientShellEndpointResponseChunkReady {
        client_id: u64,
        boot_id: String,
        request_id: String,
        final_chunk: bool,
        data: Vec<u8>,
    },
    /// A client detached gracefully.
    ClientDetach { client_id: u64 },
    /// A client connection was lost.
    ClientDisconnected { client_id: u64 },
    /// A client writer drained its render slot and can accept another render.
    ClientWriterDrained { client_id: u64 },
    /// Ctrl+C or external shutdown signal received.
    QuitSignal,
}

/// Clamp client-reported terminal dimensions to a minimum viable size.
pub(crate) fn clamp_terminal_size(cols: u16, rows: u16) -> (u16, u16) {
    let clamped_cols = cols.max(MIN_CLIENT_COLS);
    let clamped_rows = rows.max(MIN_CLIENT_ROWS);
    (clamped_cols, clamped_rows)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputEventLimit {
    WithinLimits,
    TooManyEvents,
    PasteTooLarge { size: usize },
    InputPayloadTooLarge { size: usize },
}

fn pane_input_event_limit(events: &[ClientPaneInputEvent]) -> InputEventLimit {
    let mut expanded_events = 0usize;
    let mut paste_bytes = 0usize;
    let mut input_bytes = 0usize;
    for event in events {
        expanded_events = expanded_events.saturating_add(match event {
            ClientPaneInputEvent::Key { repeat_count, .. } => usize::from((*repeat_count).max(1)),
            ClientPaneInputEvent::Mouse {
                kind:
                    crate::protocol::ClientMouseKind::ScrollUp
                    | crate::protocol::ClientMouseKind::ScrollDown,
                lines,
                ..
            } => usize::from((*lines).max(1)),
            ClientPaneInputEvent::TextCommit(_)
            | ClientPaneInputEvent::Mouse { .. }
            | ClientPaneInputEvent::Paste(_) => 1,
        });
        match event {
            ClientPaneInputEvent::Key {
                repeat_count,
                generated_text,
                ..
            } => {
                if let Some(text) = generated_text {
                    input_bytes = input_bytes.saturating_add(
                        text.len()
                            .saturating_mul(usize::from((*repeat_count).max(1))),
                    );
                }
            }
            ClientPaneInputEvent::TextCommit(text) => {
                input_bytes = input_bytes.saturating_add(text.len());
            }
            ClientPaneInputEvent::Mouse { .. } => {}
            ClientPaneInputEvent::Paste(text) => {
                paste_bytes = paste_bytes.saturating_add(text.len());
            }
        }
    }

    classify_input_event_size(expanded_events, paste_bytes, input_bytes)
}

fn classify_input_event_size(
    expanded_events: usize,
    paste_bytes: usize,
    input_bytes: usize,
) -> InputEventLimit {
    if expanded_events > MAX_INPUT_EVENT_BATCH {
        return InputEventLimit::TooManyEvents;
    }

    let payload_bytes = paste_bytes.saturating_add(input_bytes);
    if payload_bytes <= MAX_INPUT_PAYLOAD {
        InputEventLimit::WithinLimits
    } else if input_bytes == 0 {
        InputEventLimit::PasteTooLarge {
            size: payload_bytes,
        }
    } else {
        InputEventLimit::InputPayloadTooLarge {
            size: payload_bytes,
        }
    }
}

#[cfg(windows)]
fn set_client_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    context: &'static str,
    client_id: u64,
) -> io::Result<()> {
    match stream.set_recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            debug!(client_id, err = %err, context, "client socket receive timeout unavailable");
            Ok(())
        }
        Err(err) => Err(err),
    }
}

#[cfg(not(windows))]
fn set_client_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    _context: &'static str,
    _client_id: u64,
) -> io::Result<()> {
    stream.set_recv_timeout(timeout)
}

/// Handles the client handshake on a blocking thread.
///
/// Reads the `TerminalHello` or `ClientShellHello` message, validates the version,
/// sends `Welcome`, and then enters a read loop forwarding messages to the server event channel.
pub(crate) fn handle_client_handshake(
    mut stream: LocalStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    if should_quit.load(Ordering::Acquire) {
        return Ok(());
    }

    // Reset to blocking mode — the accept loop sets nonblocking but
    // the handshake thread needs blocking I/O for read_message/write_message.
    stream.set_nonblocking(false)?;

    set_client_recv_timeout(
        &stream,
        Some(HANDSHAKE_TIMEOUT),
        "client handshake read timeout unavailable",
        client_id,
    )?;

    // Read the handshake message.
    let hello: ClientMessage = match protocol::read_message(&mut stream, MAX_FRAME_SIZE) {
        Ok(msg) => msg,
        Err(protocol::FramingError::UnexpectedEof) => {
            debug!(client_id, "client disconnected before handshake");
            return Ok(());
        }
        Err(protocol::FramingError::Oversized { claimed, max }) => {
            warn!(client_id, claimed, max, "oversized handshake from client");
            return Ok(());
        }
        Err(err) => {
            debug!(client_id, err = %err, "failed to read client hello");
            return Ok(());
        }
    };

    let (
        client_cols,
        client_rows,
        cell_width_px,
        cell_height_px,
        terminal_pixel_mouse,
        shell_options,
    ) = match hello {
        ClientMessage::TerminalHello {
            version,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            pixel_mouse,
        } => {
            if let protocol::VersionCheck::Incompatible(reason) =
                protocol::check_client_version(version)
            {
                let welcome = ServerMessage::Welcome {
                    version: PROTOCOL_VERSION,
                    encoding: RenderEncoding::TerminalAnsi,
                    error: Some(reason),
                };
                let _ = protocol::write_message(&mut stream, &welcome);
                return Ok(());
            }
            let (cols, rows) = clamp_terminal_size(cols, rows);
            (cols, rows, cell_width_px, cell_height_px, pixel_mouse, None)
        }
        ClientMessage::EndpointControl { kind, data } if kind == ENDPOINT_HELLO_KIND => {
            let hello: EndpointClientHello = match serde_json::from_str(&data) {
                Ok(hello) => hello,
                Err(error) => {
                    write_endpoint_rejection(
                        &mut stream,
                        "invalid_hello",
                        format!("invalid endpoint hello: {error}"),
                    );
                    return Ok(());
                }
            };
            let incompatibility = if hello.generation != ENDPOINT_PROTOCOL_GENERATION {
                Some((
                    "unsupported_generation",
                    format!(
                        "endpoint generation {} is unsupported; this server supports generation {ENDPOINT_PROTOCOL_GENERATION}",
                        hello.generation
                    ),
                ))
            } else if !hello.supports_required_codecs() {
                Some((
                    "no_common_core",
                    "client and server have no compatible endpoint core codecs".to_owned(),
                ))
            } else {
                client_shell_geometry_error(
                    hello.surface_size,
                    hello.cell_width_px,
                    hello.cell_height_px,
                )
                .map(|reason| ("invalid_surface", reason.to_owned()))
            };
            if let Some((code, reason)) = incompatibility {
                write_endpoint_rejection(&mut stream, code, reason);
                return Ok(());
            }
            (
                hello.surface_size.cols,
                hello.surface_size.rows,
                hello.cell_width_px,
                hello.cell_height_px,
                false,
                Some((
                    hello.pixel_mouse,
                    hello.direct_graphics,
                    hello.endpoint_keybindings,
                    hello.mouse_capture,
                )),
            )
        }
        ClientMessage::ClientShellHello { .. } => {
            let welcome = ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                encoding: RenderEncoding::SemanticFrame,
                error: Some(
                    "this client predates the stable endpoint protocol; upgrade the Herdr client"
                        .to_owned(),
                ),
            };
            let _ = protocol::write_message(&mut stream, &welcome);
            return Ok(());
        }
        _ => {
            debug!(client_id, "first message was not a handshake, closing");
            let welcome = ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                encoding: RenderEncoding::SemanticFrame,
                error: Some(
                    "expected TerminalHello or ClientShellHello as first message".to_owned(),
                ),
            };
            let _ = protocol::write_message(&mut stream, &welcome);
            return Ok(());
        }
    };

    if should_quit.load(Ordering::Acquire) {
        return Ok(());
    }

    // Send the negotiated welcome. Endpoint compatibility is independent from
    // the same-install protocol used by direct terminal clients.
    let render_encoding = if shell_options.is_some() {
        RenderEncoding::SemanticFrame
    } else {
        RenderEncoding::TerminalAnsi
    };
    let welcome = if shell_options.is_some() {
        let welcome = EndpointServerWelcome::compatible(
            crate::server::client_commands::supported_client_shell_method_names()
                .iter()
                .map(|method| (*method).to_owned())
                .collect(),
        );
        ServerMessage::EndpointControl {
            kind: ENDPOINT_WELCOME_KIND.into(),
            data: serde_json::to_string(&welcome).map_err(io::Error::other)?,
        }
    } else {
        ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            encoding: render_encoding,
            error: None,
        }
    };
    protocol::write_message(&mut stream, &welcome).map_err(|e| io::Error::other(e.to_string()))?;

    set_client_recv_timeout(
        &stream,
        None,
        "failed to clear client handshake read timeout",
        client_id,
    )?;

    // Create separate channels for reliable control messages and droppable renders.
    let writer_queue = ClientWriterQueue::new();
    let writer = ClientWriter {
        control: ClientControlWriter::queue(writer_queue.clone()),
        render: ClientRenderWriter::queue(writer_queue.clone()),
    };

    // Spawn a writer thread that forwards messages from the channels to the stream.
    let write_stream = stream.try_clone()?;
    let writer_event_tx = server_event_tx.clone();
    std::thread::spawn(move || {
        client_writer_loop(write_stream, client_id, writer_queue, writer_event_tx);
    });

    if should_quit.load(Ordering::Acquire) {
        send_shutdown_to_unregistered_client(&writer);
        return Ok(());
    }

    // Notify the main loop about the new client.
    let connected =
        if let Some((pixel_mouse, direct_graphics, endpoint_keybindings, mouse_capture)) =
            shell_options
        {
            ServerEvent::ClientShellConnected {
                client_id,
                surface_cols: client_cols,
                surface_rows: client_rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
                direct_graphics,
                endpoint_keybindings,
                mouse_capture,
                writer,
            }
        } else {
            ServerEvent::ClientConnected {
                client_id,
                cols: client_cols,
                rows: client_rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse: terminal_pixel_mouse,
                writer,
            }
        };
    if let Err(err) = server_event_tx.blocking_send(connected) {
        match err.0 {
            ServerEvent::ClientConnected { writer, .. }
            | ServerEvent::ClientShellConnected { writer, .. } => {
                send_shutdown_to_unregistered_client(&writer);
            }
            _ => {}
        }
    }

    // Enter read loop — read client messages and forward to main loop.
    client_read_loop(stream, client_id, server_event_tx, should_quit)
}

fn send_shutdown_to_unregistered_client(writer: &ClientWriter) {
    let mut framed = Vec::new();
    if protocol::write_message(
        &mut framed,
        &ServerMessage::ServerShutdown {
            reason: Some("server is shutting down".to_owned()),
        },
    )
    .is_ok()
    {
        let _ = writer.control.send(framed);
    }
}

/// The client writer loop — prioritizes control messages over render frames.
fn client_writer_loop(
    mut stream: LocalStream,
    client_id: u64,
    writer_queue: Arc<ClientWriterQueue>,
    server_event_tx: mpsc::Sender<ServerEvent>,
) {
    while let Some(item) = writer_queue.recv() {
        let written = match item {
            ClientWriteItem::Control(data) => write_framed_bytes(&mut stream, &data),
            ClientWriteItem::Render(data) => {
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientWriterDrained { client_id });
                write_framed_bytes(&mut stream, &data)
            }
        };
        if !written {
            let _ = server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
            break;
        }
    }
    writer_queue.close_writer();
    debug!("client writer thread exiting");
}

fn write_framed_bytes(stream: &mut LocalStream, data: &[u8]) -> bool {
    if let Err(err) = stream.write_all(data) {
        debug!(err = %err, "client write failed, closing writer");
        return false;
    }
    if let Err(err) = stream.flush() {
        debug!(err = %err, "client flush failed, closing writer");
        return false;
    }
    true
}

/// The client read loop — reads messages from the client and forwards to the server event channel.
fn client_read_loop(
    mut stream: LocalStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    while !should_quit.load(Ordering::Acquire) {
        let msg: ClientMessage = match protocol::read_message(&mut stream, MAX_GRAPHICS_FRAME_SIZE)
        {
            Ok(msg) => msg,
            Err(protocol::FramingError::UnexpectedEof) => {
                // Client disconnected.
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
            Err(protocol::FramingError::Oversized { claimed, max }) => {
                warn!(
                    client_id,
                    claimed, max, "oversized message from client, closing"
                );
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
            Err(err) => {
                debug!(client_id, err = %err, "client read error, closing");
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
        };

        let event = match msg {
            ClientMessage::Input { data } => {
                // Validate input size.
                if data.len() > MAX_INPUT_PAYLOAD {
                    if crate::raw_input::is_complete_text_bracketed_paste(&data) {
                        warn!(
                            client_id,
                            size = data.len(),
                            max = MAX_INPUT_PAYLOAD,
                            "oversized bracketed paste from client, rejecting"
                        );
                        ServerEvent::ClientPasteRejected {
                            client_id,
                            size: data.len(),
                            max: MAX_INPUT_PAYLOAD,
                        }
                    } else {
                        warn!(
                            client_id,
                            size = data.len(),
                            "oversized input from client, closing"
                        );
                        let _ = server_event_tx
                            .blocking_send(ServerEvent::ClientDisconnected { client_id });
                        break;
                    }
                } else {
                    ServerEvent::ClientInput { client_id, data }
                }
            }
            ClientMessage::ObserveTerminal { target } => {
                ServerEvent::ClientObserveTerminal { client_id, target }
            }
            ClientMessage::ControlTerminal { target, takeover } => {
                ServerEvent::ClientControlTerminal {
                    client_id,
                    target,
                    takeover,
                }
            }
            ClientMessage::GraphicsTransmissionResult {
                transfer_id,
                image_id,
                success,
            } => ServerEvent::GraphicsTransmissionResult {
                client_id,
                transfer_id,
                image_id,
                success,
            },
            ClientMessage::GraphicsTransmissionStarted {
                transfer_id,
                image_id,
            } => ServerEvent::GraphicsTransmissionStarted {
                client_id,
                transfer_id,
                image_id,
            },
            ClientMessage::ClipboardImage {
                target,
                extension,
                data,
            } => {
                if data.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
                    warn!(
                        client_id,
                        size = data.len(),
                        "oversized clipboard image from client, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                } else {
                    ServerEvent::ClientClipboardImage {
                        client_id,
                        target,
                        extension,
                        data,
                    }
                }
            }
            ClientMessage::Resize {
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
            } => {
                let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
                ServerEvent::ClientResize {
                    client_id,
                    cols: clamped_cols,
                    rows: clamped_rows,
                    cell_width_px,
                    cell_height_px,
                    pixel_mouse,
                }
            }
            ClientMessage::ClientShellResize {
                cell_width_px,
                cell_height_px,
                surface_size,
                pixel_mouse,
            } => {
                if let Some(reason) =
                    client_shell_geometry_error(surface_size, cell_width_px, cell_height_px)
                {
                    warn!(client_id, %reason, "invalid client shell resize, closing");
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                }
                ServerEvent::ClientShellResize {
                    client_id,
                    surface_cols: surface_size.cols,
                    surface_rows: surface_size.rows,
                    cell_width_px,
                    cell_height_px,
                    pixel_mouse,
                }
            }
            ClientMessage::ClientShellHostTheme { update } => {
                if matches!(
                    &update,
                    crate::protocol::ClientHostThemeUpdate::PaletteColors(colors)
                        if colors.len() > 256
                ) {
                    warn!(client_id, "invalid client shell host theme update, closing");
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                }
                ServerEvent::ClientShellHostTheme { client_id, update }
            }
            ClientMessage::ClientShellFocus { focused } => {
                ServerEvent::ClientShellFocus { client_id, focused }
            }
            ClientMessage::ClientShellMouseCapture { enabled } => {
                ServerEvent::ClientShellMouseCapture { client_id, enabled }
            }
            ClientMessage::ClientShellPaneInput { pane_id, events } => {
                match pane_input_event_limit(&events) {
                    InputEventLimit::WithinLimits => ServerEvent::ClientShellPaneInput {
                        client_id,
                        pane_id,
                        events,
                    },
                    InputEventLimit::TooManyEvents => {
                        warn!(
                            client_id,
                            count = events.len(),
                            "oversized targeted pane input batch, closing"
                        );
                        let _ = server_event_tx
                            .blocking_send(ServerEvent::ClientDisconnected { client_id });
                        break;
                    }
                    InputEventLimit::PasteTooLarge { size } => {
                        warn!(
                            client_id,
                            size,
                            max = MAX_INPUT_PAYLOAD,
                            "oversized targeted pane paste, rejecting"
                        );
                        ServerEvent::ClientPasteRejected {
                            client_id,
                            size,
                            max: MAX_INPUT_PAYLOAD,
                        }
                    }
                    InputEventLimit::InputPayloadTooLarge { size } => {
                        warn!(
                            client_id,
                            size,
                            max = MAX_INPUT_PAYLOAD,
                            "oversized targeted pane input, closing"
                        );
                        let _ = server_event_tx
                            .blocking_send(ServerEvent::ClientDisconnected { client_id });
                        break;
                    }
                }
            }
            ClientMessage::ClientShellPopupInput {
                terminal_id,
                events,
            } => match pane_input_event_limit(&events) {
                InputEventLimit::WithinLimits => ServerEvent::ClientShellPopupInput {
                    client_id,
                    terminal_id,
                    events,
                },
                InputEventLimit::TooManyEvents => {
                    warn!(
                        client_id,
                        count = events.len(),
                        "oversized popup input batch, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                }
                InputEventLimit::PasteTooLarge { size } => {
                    warn!(
                        client_id,
                        size,
                        max = MAX_INPUT_PAYLOAD,
                        "oversized popup paste, rejecting"
                    );
                    ServerEvent::ClientPasteRejected {
                        client_id,
                        size,
                        max: MAX_INPUT_PAYLOAD,
                    }
                }
                InputEventLimit::InputPayloadTooLarge { size } => {
                    warn!(
                        client_id,
                        size,
                        max = MAX_INPUT_PAYLOAD,
                        "oversized popup input, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                }
            },
            ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                if boot_id.len() > crate::server::client_commands::MAX_ENDPOINT_BOOT_ID_BYTES
                    || request.len() > crate::server::client_commands::MAX_ENDPOINT_COMMAND_BYTES
                {
                    warn!(
                        client_id,
                        boot_id_size = boot_id.len(),
                        request_size = request.len(),
                        "oversized client shell endpoint command, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                }
                let decoded = match decode_endpoint_request(&request) {
                    Ok(decoded) => decoded,
                    Err(error) => {
                        warn!(client_id, %error, "invalid endpoint request envelope, closing");
                        let _ = server_event_tx
                            .blocking_send(ServerEvent::ClientDisconnected { client_id });
                        break;
                    }
                };
                let request_id = match &decoded {
                    DecodedEndpointRequest::Dispatch(request) => request.id.as_str(),
                    DecodedEndpointRequest::Error { request_id, .. } => request_id,
                };
                if request_id.len() > crate::server::client_commands::MAX_ENDPOINT_REQUEST_ID_BYTES
                {
                    warn!(
                        client_id,
                        "oversized client shell endpoint request id, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                }
                match decoded {
                    DecodedEndpointRequest::Dispatch(request) => {
                        ServerEvent::ClientShellEndpointRequest {
                            client_id,
                            boot_id,
                            request,
                        }
                    }
                    DecodedEndpointRequest::Error {
                        request_id,
                        code,
                        message,
                    } => ServerEvent::ClientShellEndpointRequestError {
                        client_id,
                        boot_id,
                        request_id,
                        code,
                        message,
                    },
                }
            }
            ClientMessage::EndpointControl { kind, .. } => {
                debug!(client_id, %kind, "ignoring unknown endpoint control message");
                continue;
            }
            ClientMessage::Detach => {
                let _ = server_event_tx.blocking_send(ServerEvent::ClientDetach { client_id });
                break;
            }
            ClientMessage::AttachTerminal {
                terminal_id,
                takeover,
            } => ServerEvent::ClientAttachTerminal {
                client_id,
                terminal_id,
                takeover,
            },
            ClientMessage::AttachScroll {
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            } => ServerEvent::ClientAttachScroll {
                client_id,
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            },
            ClientMessage::AttachMouse {
                kind,
                position,
                geometry,
                modifiers,
                lines,
            } => ServerEvent::ClientAttachMouse {
                client_id,
                kind,
                position,
                geometry,
                modifiers,
                lines,
            },
            ClientMessage::TerminalHello { .. } | ClientMessage::ClientShellHello { .. } => {
                // Duplicate handshake — ignore.
                continue;
            }
        };

        if server_event_tx.blocking_send(event).is_err() {
            break; // Main loop gone.
        }
    }

    debug!(client_id, "client read thread exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::traits::Listener as _;
    use std::path::PathBuf;

    struct TestSocketPath(PathBuf);

    impl Drop for TestSocketPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn unique_test_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let filename = format!("h{}-{nanos}.sock", std::process::id());
        #[cfg(unix)]
        {
            let _ = name;
            PathBuf::from("/tmp").join(filename)
        }
        #[cfg(windows)]
        {
            std::env::temp_dir().join(format!("herdr-{name}-{filename}"))
        }
    }

    fn local_stream_pair(name: &str) -> (LocalStream, LocalStream, TestSocketPath) {
        let path = unique_test_path(name);
        let _ = std::fs::remove_file(&path);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, TestSocketPath(path))
    }

    fn endpoint_hello(surface_cols: u16, surface_rows: u16) -> ClientMessage {
        let hello = EndpointClientHello {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: crate::protocol::ClientSurfaceSize {
                cols: surface_cols,
                rows: surface_rows,
            },
            pixel_mouse: true,
            direct_graphics: true,
            endpoint_keybindings: true,
            mouse_capture: true,
            snapshot_codecs: vec![crate::protocol::endpoint::SNAPSHOT_CODEC_V1.into()],
            surface_codecs: vec![crate::protocol::endpoint::SURFACE_CODEC_V1.into()],
            input_codecs: vec![crate::protocol::endpoint::INPUT_CODEC_V1.into()],
            blob_codecs: vec![crate::protocol::endpoint::BLOB_CODEC_V1.into()],
        };
        ClientMessage::EndpointControl {
            kind: ENDPOINT_HELLO_KIND.into(),
            data: serde_json::to_string(&hello).unwrap(),
        }
    }

    fn endpoint_welcome(message: ServerMessage) -> EndpointServerWelcome {
        let ServerMessage::EndpointControl { kind, data } = message else {
            panic!("expected endpoint welcome");
        };
        assert_eq!(kind, ENDPOINT_WELCOME_KIND);
        serde_json::from_str(&data).unwrap()
    }

    fn recv_server_event(receiver: &mut mpsc::Receiver<ServerEvent>, context: &str) -> ServerEvent {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match receiver.try_recv() {
                Ok(event) => return event,
                Err(mpsc::error::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(err) => panic!("{context}: {err}"),
            }
        }
    }

    fn bracketed_paste_with_total_len(total_len: usize) -> Vec<u8> {
        const DELIMITER_BYTES: usize = b"\x1b[200~".len() + b"\x1b[201~".len();
        assert!(total_len >= DELIMITER_BYTES);
        let mut data = Vec::with_capacity(total_len);
        data.extend_from_slice(b"\x1b[200~");
        data.resize(total_len - b"\x1b[201~".len(), b'x');
        data.extend_from_slice(b"\x1b[201~");
        data
    }

    fn test_queue_writer() -> (ClientWriter, Arc<ClientWriterQueue>) {
        let queue = ClientWriterQueue::new();
        (
            ClientWriter {
                control: ClientControlWriter::queue(queue.clone()),
                render: ClientRenderWriter::queue(queue.clone()),
            },
            queue,
        )
    }

    fn frame_server_message(message: &ServerMessage) -> Vec<u8> {
        let mut bytes = Vec::new();
        protocol::write_message(&mut bytes, message).expect("frame server message");
        bytes
    }

    #[test]
    fn client_writer_queue_keeps_render_slot_bounded() {
        let (writer, _queue) = test_queue_writer();
        let first = frame_server_message(&ServerMessage::WindowTitle {
            title: Some("first".into()),
        });
        let second = frame_server_message(&ServerMessage::WindowTitle {
            title: Some("second".into()),
        });

        writer.render.try_send(first).expect("first render fits");
        assert!(matches!(
            writer.render.try_send(second),
            Err(TrySendError::Full(_))
        ));
    }

    #[test]
    fn ordered_direct_follows_older_render_and_stays_bounded() {
        let (writer, queue) = test_queue_writer();
        writer.render.try_send(b"old".to_vec()).unwrap();
        writer.render.send_ordered(b"direct".to_vec()).unwrap();
        assert!(matches!(
            writer.render.send_ordered(b"second".to_vec()),
            Err(TrySendError::Full(_))
        ));
        writer.render.try_send(b"new".to_vec()).unwrap();

        for expected in [b"old".as_slice(), b"direct", b"new"] {
            assert_eq!(
                queue.recv(),
                Some(ClientWriteItem::Render(expected.to_vec()))
            );
        }
        queue.close_writer();
        assert!(matches!(
            writer.render.send_ordered(b"closed".to_vec()),
            Err(TrySendError::Disconnected(_))
        ));
    }

    #[test]
    fn client_writer_prioritizes_control_and_reports_render_drain() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-writer-priority");
        let (writer, queue) = test_queue_writer();
        writer
            .render
            .try_send(frame_server_message(&ServerMessage::WindowTitle {
                title: Some("render".into()),
            }))
            .expect("queue render");
        writer
            .control
            .send(frame_server_message(&ServerMessage::ReloadSoundConfig))
            .expect("queue control");

        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let handle = std::thread::spawn(move || {
            client_writer_loop(server_stream, 9, queue, server_event_tx);
        });

        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read control") {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected control message first, got {other:?}"),
        }
        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read render") {
            ServerMessage::WindowTitle { title } => assert_eq!(title.as_deref(), Some("render")),
            other => panic!("expected render message second, got {other:?}"),
        }
        match server_event_rx
            .blocking_recv()
            .expect("writer drained render slot")
        {
            ServerEvent::ClientWriterDrained { client_id } => assert_eq!(client_id, 9),
            other => panic!("expected writer drained event, got {other:?}"),
        }

        drop(writer);
        handle.join().expect("writer exits after senders drop");
    }

    #[test]
    fn client_writer_exits_when_all_writer_handles_drop() {
        let (_client_stream, server_stream, _path) = local_stream_pair("client-writer-drop");
        let (writer, queue) = test_queue_writer();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 11, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(writer);
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer exits without polling after senders drop");
    }

    #[test]
    fn client_writer_clone_keeps_loop_alive_until_final_drop() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-writer-clone-drop");
        let (writer, queue) = test_queue_writer();
        let cloned_writer = writer.clone();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 12, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(writer);
        cloned_writer
            .control
            .send(frame_server_message(&ServerMessage::ReloadSoundConfig))
            .expect("cloned writer still sends after original drops");
        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE)
            .expect("read control from cloned writer")
        {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected cloned control message, got {other:?}"),
        }
        assert!(
            done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "writer exited while cloned handles were still alive"
        );

        drop(cloned_writer);
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer exits after final cloned writer drops");
    }

    #[test]
    fn client_writer_closes_queue_after_socket_write_failure() {
        let (client_stream, server_stream, _path) =
            local_stream_pair("client-writer-socket-failure");
        #[cfg(not(windows))]
        server_stream
            .set_send_timeout(Some(Duration::from_millis(100)))
            .expect("set test send timeout");
        let (writer, queue) = test_queue_writer();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 13, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(client_stream);
        writer
            .control
            .send(vec![b'x'; 1024 * 1024])
            .expect("message is accepted before the writer observes socket failure");
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer exits after socket write failure");

        assert!(matches!(writer.control.send(vec![b'y']), Err(SendError(_))));
        assert!(matches!(
            writer.render.try_send(vec![b'z']),
            Err(TrySendError::Disconnected(_))
        ));
    }

    #[test]
    fn clamp_terminal_size_zero_zero() {
        assert_eq!(
            clamp_terminal_size(0, 0),
            (MIN_CLIENT_COLS, MIN_CLIENT_ROWS)
        );
    }

    #[test]
    fn clamp_terminal_size_one_one() {
        assert_eq!(clamp_terminal_size(1, 1), (1, 1));
    }

    #[test]
    fn clamp_terminal_size_preserves_narrow_client_size() {
        assert_eq!(clamp_terminal_size(40, 12), (40, 12));
    }

    #[test]
    fn clamp_terminal_size_valid() {
        assert_eq!(clamp_terminal_size(120, 40), (120, 40));
    }

    #[test]
    fn clamp_terminal_size_exact_minimum() {
        assert_eq!(
            clamp_terminal_size(MIN_CLIENT_COLS, MIN_CLIENT_ROWS),
            (MIN_CLIENT_COLS, MIN_CLIENT_ROWS)
        );
    }

    #[test]
    fn client_shell_geometry_rejects_unsafe_dimensions_and_cell_sizes() {
        assert!(client_shell_geometry_error(
            crate::protocol::ClientSurfaceSize { cols: 80, rows: 24 },
            8,
            16,
        )
        .is_none());
        assert!(client_shell_geometry_error(
            crate::protocol::ClientSurfaceSize {
                cols: MAX_CLIENT_SHELL_DIMENSION,
                rows: MAX_CLIENT_SHELL_DIMENSION,
            },
            8,
            16,
        )
        .is_some());
        assert!(client_shell_geometry_error(
            crate::protocol::ClientSurfaceSize { cols: 80, rows: 24 },
            MAX_CLIENT_CELL_SIZE_PX + 1,
            16,
        )
        .is_some());
    }

    #[test]
    fn unknown_endpoint_method_returns_correlated_error() {
        let decoded = decode_endpoint_request(
            r#"{"id":"req-1","method":"plugin.future","params":{"value":1}}"#,
        )
        .unwrap();
        assert!(matches!(
            decoded,
            DecodedEndpointRequest::Error {
                request_id,
                code: "unsupported_method",
                ..
            } if request_id == "req-1"
        ));
    }

    #[test]
    fn malformed_known_endpoint_method_returns_correlated_error() {
        let decoded =
            decode_endpoint_request(r#"{"id":"req-2","method":"workspace.focus","params":{}}"#)
                .unwrap();
        assert!(matches!(
            decoded,
            DecodedEndpointRequest::Error {
                request_id,
                code: "invalid_request",
                ..
            } if request_id == "req-2"
        ));
    }

    #[test]
    fn handshake_negotiates_terminal_ansi_encoding() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-handshake-ansi");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let handshake_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            handle_client_handshake(server_stream, 42, &server_event_tx, &handshake_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::TerminalHello {
                version: PROTOCOL_VERSION,
                cols: 100,
                rows: 30,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: true,
            },
        )
        .expect("write hello");

        let welcome: ServerMessage =
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read welcome");
        match welcome {
            ServerMessage::Welcome {
                version,
                encoding,
                error,
            } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert_eq!(encoding, RenderEncoding::TerminalAnsi);
                assert_eq!(error, None);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }

        match server_event_rx
            .blocking_recv()
            .expect("client connected event")
        {
            ServerEvent::ClientConnected {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
                writer,
            } => {
                assert_eq!(client_id, 42);
                assert_eq!((cols, rows), (100, 30));
                assert_eq!((cell_width_px, cell_height_px), (8, 16));
                assert!(pixel_mouse);
                drop(writer);
            }
            other => panic!("expected ClientConnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("handshake thread join")
            .expect("handshake thread result");
    }

    #[test]
    fn dedicated_client_shell_handshake_uses_surface_viewport() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-shell-handshake");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let handshake_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            handle_client_handshake(server_stream, 43, &server_event_tx, &handshake_quit)
        });

        protocol::write_message(&mut client_stream, &endpoint_hello(80, 29))
            .expect("write shell hello");

        let welcome: ServerMessage =
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read welcome");
        let welcome = endpoint_welcome(welcome);
        assert_eq!(welcome.generation, ENDPOINT_PROTOCOL_GENERATION);
        assert!(welcome.error.is_none());
        match server_event_rx
            .blocking_recv()
            .expect("client shell connected event")
        {
            ServerEvent::ClientShellConnected {
                client_id,
                surface_cols,
                surface_rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
                direct_graphics,
                endpoint_keybindings,
                mouse_capture,
                writer,
            } => {
                assert_eq!(client_id, 43);
                assert_eq!((surface_cols, surface_rows), (80, 29));
                assert_eq!((cell_width_px, cell_height_px), (8, 16));
                assert!(pixel_mouse);
                assert!(direct_graphics);
                assert!(endpoint_keybindings);
                assert!(mouse_capture);
                drop(writer);
            }
            other => panic!("expected ClientShellConnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("handshake thread join")
            .expect("handshake thread result");
    }

    #[test]
    fn dedicated_client_shell_handshake_rejects_empty_surface() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-shell-empty-surface");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let handshake_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            handle_client_handshake(server_stream, 43, &server_event_tx, &handshake_quit)
        });

        protocol::write_message(&mut client_stream, &endpoint_hello(0, 29))
            .expect("write empty shell hello");

        let welcome: ServerMessage =
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read welcome");
        let welcome = endpoint_welcome(welcome);
        assert!(welcome
            .error
            .is_some_and(|error| error.message.contains("non-empty pane surface")));
        handle
            .join()
            .expect("handshake thread join")
            .expect("handshake thread result");
        assert!(server_event_rx.try_recv().is_err());
    }

    #[test]
    fn client_read_loop_stops_after_detach() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-detach");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        let mut messages = Vec::new();
        protocol::write_message(&mut messages, &ClientMessage::Detach).unwrap();
        protocol::write_message(
            &mut messages,
            &ClientMessage::ClipboardImage {
                target: crate::protocol::ClientClipboardImageTarget::DirectTerminal,
                extension: "png".into(),
                data: vec![1, 2, 3],
            },
        )
        .unwrap();
        client_stream
            .write_all(&messages)
            .expect("write detach and trailing message");

        assert!(matches!(
            recv_server_event(&mut server_event_rx, "detach event"),
            ServerEvent::ClientDetach { client_id: 7 }
        ));
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
        assert!(server_event_rx.try_recv().is_err());
    }

    #[test]
    fn client_read_loop_ignores_unknown_endpoint_control() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-future-control");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::EndpointControl {
                kind: "future.optional.v1".into(),
                data: "{}".into(),
            },
        )
        .unwrap();
        protocol::write_message(&mut client_stream, &ClientMessage::Detach).unwrap();

        assert!(matches!(
            recv_server_event(&mut server_event_rx, "detach after future control"),
            ServerEvent::ClientDetach { client_id: 7 }
        ));
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_closes_on_unsafe_shell_resize() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-unsafe-resize");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::ClientShellResize {
                cell_width_px: 8,
                cell_height_px: 16,
                surface_size: crate::protocol::ClientSurfaceSize {
                    cols: MAX_CLIENT_SHELL_DIMENSION,
                    rows: MAX_CLIENT_SHELL_DIMENSION,
                },
                pixel_mouse: false,
            },
        )
        .unwrap();

        assert!(matches!(
            recv_server_event(&mut server_event_rx, "unsafe resize disconnect"),
            ServerEvent::ClientDisconnected { client_id: 7 }
        ));
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_rejects_oversized_bracketed_paste_without_disconnect() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-oversized");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD),
            },
        )
        .expect("write maximum-size bracketed paste");

        match recv_server_event(&mut server_event_rx, "maximum-size paste event") {
            ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 7);
                assert_eq!(data.len(), MAX_INPUT_PAYLOAD);
            }
            other => panic!("expected maximum-size ClientInput, got {other:?}"),
        }

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1),
            },
        )
        .expect("write oversized bracketed paste");

        match recv_server_event(&mut server_event_rx, "oversized paste rejection") {
            ServerEvent::ClientPasteRejected {
                client_id,
                size,
                max,
            } => {
                assert_eq!(client_id, 7);
                assert_eq!(size, MAX_INPUT_PAYLOAD + 1);
                assert_eq!(max, MAX_INPUT_PAYLOAD);
            }
            ServerEvent::ClientDisconnected { .. } => {
                panic!("oversized input must be rejected without disconnecting the client")
            }
            other => panic!("expected ClientPasteRejected, got {other:?}"),
        }

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: b"still connected".to_vec(),
            },
        )
        .expect("write valid input after rejection");

        match recv_server_event(&mut server_event_rx, "valid input after rejection") {
            ServerEvent::ClientInput { client_id, data } => {
                assert_eq!(client_id, 7);
                assert_eq!(data, b"still connected");
            }
            other => panic!("expected ClientInput after rejection, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_disconnects_oversized_non_paste_input() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-oversized-non-paste");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: vec![b'x'; MAX_INPUT_PAYLOAD + 1],
            },
        )
        .expect("write oversized non-paste input");

        assert!(matches!(
            recv_server_event(&mut server_event_rx, "oversized non-paste disconnect"),
            ServerEvent::ClientDisconnected { client_id: 7 }
        ));

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_disconnects_marker_wrapped_invalid_utf8() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-invalid-utf8-paste");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });
        let mut data = bracketed_paste_with_total_len(MAX_INPUT_PAYLOAD + 1);
        data[b"\x1b[200~".len()] = 0xff;

        protocol::write_message(&mut client_stream, &ClientMessage::Input { data })
            .expect("write marker-wrapped invalid UTF-8 input");

        assert!(matches!(
            recv_server_event(&mut server_event_rx, "invalid UTF-8 input disconnect"),
            ServerEvent::ClientDisconnected { client_id: 7 }
        ));

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_uses_authoritative_shell_resize_surface() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-resize");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::ClientShellResize {
                cell_width_px: 8,
                cell_height_px: 16,
                surface_size: crate::protocol::ClientSurfaceSize { cols: 60, rows: 15 },
                pixel_mouse: true,
            },
        )
        .expect("write shell resize");
        assert!(matches!(
            recv_server_event(&mut server_event_rx, "shell resize"),
            ServerEvent::ClientShellResize {
                client_id: 7,
                surface_cols: 60,
                surface_rows: 15,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: true,
            }
        ));

        protocol::write_message(&mut client_stream, &ClientMessage::Detach).expect("write detach");
        assert!(matches!(
            recv_server_event(&mut server_event_rx, "detach event"),
            ServerEvent::ClientDetach { client_id: 7 }
        ));
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_keeps_single_host_theme_updates_ordered_and_palette_bounded() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-host-theme");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        let colors = (0..=u8::MAX)
            .map(|index| {
                (
                    index,
                    crate::protocol::ClientHostColor {
                        r: index,
                        g: 0,
                        b: 0,
                    },
                )
            })
            .collect();
        protocol::write_message(
            &mut client_stream,
            &ClientMessage::ClientShellHostTheme {
                update: crate::protocol::ClientHostThemeUpdate::PaletteColors(colors),
            },
        )
        .expect("write bounded palette update");
        protocol::write_message(
            &mut client_stream,
            &ClientMessage::ClientShellHostTheme {
                update: crate::protocol::ClientHostThemeUpdate::Appearance(
                    crate::protocol::ClientHostAppearance::Dark,
                ),
            },
        )
        .expect("write ordered appearance update");

        assert!(matches!(
            recv_server_event(&mut server_event_rx, "bounded palette update"),
            ServerEvent::ClientShellHostTheme {
                client_id: 7,
                update: crate::protocol::ClientHostThemeUpdate::PaletteColors(colors),
            } if colors.len() == 256
        ));
        assert!(matches!(
            recv_server_event(&mut server_event_rx, "ordered appearance update"),
            ServerEvent::ClientShellHostTheme {
                client_id: 7,
                update: crate::protocol::ClientHostThemeUpdate::Appearance(
                    crate::protocol::ClientHostAppearance::Dark
                ),
            }
        ));

        let colors = vec![(0, crate::protocol::ClientHostColor { r: 0, g: 0, b: 0 },); 257];
        protocol::write_message(
            &mut client_stream,
            &ClientMessage::ClientShellHostTheme {
                update: crate::protocol::ClientHostThemeUpdate::PaletteColors(colors),
            },
        )
        .expect("write oversized palette update");
        assert!(matches!(
            recv_server_event(&mut server_event_rx, "oversized palette disconnect"),
            ServerEvent::ClientDisconnected { client_id: 7 }
        ));

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn pane_input_limits_charge_scroll_repeats() {
        let oversized_scroll = ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::ScrollUp,
            position: crate::protocol::ClientMousePosition::Cell { column: 0, row: 0 },
            geometry: None,
            modifiers: 0,
            lines: (MAX_INPUT_EVENT_BATCH + 1) as u16,
        };
        assert_eq!(
            pane_input_event_limit(&[oversized_scroll]),
            InputEventLimit::TooManyEvents
        );
    }

    #[test]
    fn handshake_timeout_is_within_five_second_deadline() {
        // The handshake timeout must be short enough that
        // the connection is guaranteed to close within 5 seconds even with
        // OS overhead (thread scheduling, timer slack, cleanup).
        assert!(
            HANDSHAKE_TIMEOUT < Duration::from_secs(5),
            "HANDSHAKE_TIMEOUT ({:?}) must be less than 5 seconds to guarantee \
             connection close within the 5-second deadline",
            HANDSHAKE_TIMEOUT
        );
    }
}
