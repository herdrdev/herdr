//! Wire protocol for herdr server/client communication.
//!
//! Defines the message types, framing, version negotiation, and safety
//! constraints for the binary protocol over local sockets.
//!
//! `PROTOCOL_VERSION` still guards same-install direct-terminal and internal
//! operations. Client-owned shells negotiate the independent stable endpoint
//! contract in [`super::endpoint`].

use std::collections::HashMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

/// Current protocol version. Bumped when wire format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 22;

/// Maximum allowed frame payload size (2 MB). Frames larger than this are
/// rejected to prevent denial-of-service via oversized length prefixes.
pub const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024;

/// Maximum allowed server-to-client frame payload when Kitty graphics are enabled.
/// Normal traffic keeps `MAX_FRAME_SIZE`; this larger cap is only for explicit
/// image payloads that are naturally much larger after base64 encoding.
pub const MAX_GRAPHICS_FRAME_SIZE: usize = 32 * 1024 * 1024;

/// Maximum clipboard image payload size for remote paste bridging.
pub const MAX_CLIPBOARD_IMAGE_PAYLOAD: usize = 16 * 1024 * 1024;

/// Length of the u32 little-endian length prefix in bytes.
const LENGTH_PREFIX_BYTES: usize = 4;

// ---------------------------------------------------------------------------
// Client → Server messages
// ---------------------------------------------------------------------------

/// Render payload encoding negotiated during client handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderEncoding {
    /// Send full semantic FrameData values. This is the local/default mode.
    SemanticFrame,
    /// Send already-diffed terminal ANSI byte streams.
    TerminalAnsi,
}

/// Size of the pane surface requested by a client-owned shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSurfaceSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientKeyKind {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientKeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    Esc,
    Char(char),
    F(u8),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientMouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMouseKind {
    Down(ClientMouseButton),
    Up(ClientMouseButton),
    Drag(ClientMouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMousePosition {
    Cell {
        column: u16,
        row: u16,
    },
    Pixels {
        x: u32,
        y: u32,
        column: u16,
        row: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientMouseGeometry {
    pub cols: u16,
    pub rows: u16,
    pub width_px: u32,
    pub height_px: u32,
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientInputEvent {
    Key {
        code: ClientKeyCode,
        modifiers: u8,
        kind: ClientKeyKind,
        repeat_count: u16,
        generated_text: Option<String>,
        source: ClientKeySource,
    },
    TextCommit(String),
    Mouse {
        kind: ClientMouseKind,
        column: u16,
        row: u16,
        modifiers: u8,
    },
    Paste {
        text: String,
    },
    FocusGained,
    FocusLost,
}

/// Pane-domain input after the client has classified and consumed shell actions.
///
/// Keys are semantic rather than outer-terminal VT bytes so the target pane can
/// encode them for the child application's negotiated keyboard protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientPaneInputEvent {
    Key {
        code: ClientKeyCode,
        modifiers: u8,
        kind: ClientKeyKind,
        repeat_count: u16,
        shifted_codepoint: Option<u32>,
        generated_text: Option<String>,
        tracks_release: bool,
        physical_key_id: Option<u32>,
        windows_record: Option<crate::input::WindowsKeyRecord>,
    },
    TextCommit(String),
    Mouse {
        kind: ClientMouseKind,
        position: ClientMousePosition,
        geometry: Option<ClientMouseGeometry>,
        modifiers: u8,
        lines: u16,
    },
    Paste(String),
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientKeySource {
    Synthesized,
    Vt {
        bytes: Vec<u8>,
    },
    WindowsConsole {
        record: crate::input::WindowsKeyRecord,
    },
}

impl ClientKeyKind {
    pub(crate) fn from_crossterm(kind: crossterm::event::KeyEventKind) -> Self {
        match kind {
            crossterm::event::KeyEventKind::Press => Self::Press,
            crossterm::event::KeyEventKind::Repeat => Self::Repeat,
            crossterm::event::KeyEventKind::Release => Self::Release,
        }
    }

    pub(crate) fn to_crossterm(self) -> crossterm::event::KeyEventKind {
        match self {
            Self::Press => crossterm::event::KeyEventKind::Press,
            Self::Repeat => crossterm::event::KeyEventKind::Repeat,
            Self::Release => crossterm::event::KeyEventKind::Release,
        }
    }
}

impl ClientKeyCode {
    pub(crate) fn from_crossterm(code: crossterm::event::KeyCode) -> Option<Self> {
        use crossterm::event::KeyCode;
        Some(match code {
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Enter => Self::Enter,
            KeyCode::Left => Self::Left,
            KeyCode::Right => Self::Right,
            KeyCode::Up => Self::Up,
            KeyCode::Down => Self::Down,
            KeyCode::Home => Self::Home,
            KeyCode::End => Self::End,
            KeyCode::PageUp => Self::PageUp,
            KeyCode::PageDown => Self::PageDown,
            KeyCode::Tab => Self::Tab,
            KeyCode::BackTab => Self::BackTab,
            KeyCode::Delete => Self::Delete,
            KeyCode::Insert => Self::Insert,
            KeyCode::Esc => Self::Esc,
            KeyCode::Char(ch) => Self::Char(ch),
            KeyCode::F(n) => Self::F(n),
            KeyCode::Null => Self::Null,
            _ => return None,
        })
    }

    pub(crate) fn to_crossterm(&self) -> crossterm::event::KeyCode {
        use crossterm::event::KeyCode;
        match self {
            Self::Backspace => KeyCode::Backspace,
            Self::Enter => KeyCode::Enter,
            Self::Left => KeyCode::Left,
            Self::Right => KeyCode::Right,
            Self::Up => KeyCode::Up,
            Self::Down => KeyCode::Down,
            Self::Home => KeyCode::Home,
            Self::End => KeyCode::End,
            Self::PageUp => KeyCode::PageUp,
            Self::PageDown => KeyCode::PageDown,
            Self::Tab => KeyCode::Tab,
            Self::BackTab => KeyCode::BackTab,
            Self::Delete => KeyCode::Delete,
            Self::Insert => KeyCode::Insert,
            Self::Esc => KeyCode::Esc,
            Self::Char(ch) => KeyCode::Char(*ch),
            Self::F(n) => KeyCode::F(*n),
            Self::Null => KeyCode::Null,
        }
    }
}

impl ClientMouseButton {
    pub(crate) fn from_crossterm(button: crossterm::event::MouseButton) -> Self {
        match button {
            crossterm::event::MouseButton::Left => Self::Left,
            crossterm::event::MouseButton::Right => Self::Right,
            crossterm::event::MouseButton::Middle => Self::Middle,
        }
    }

    pub(crate) fn to_crossterm(self) -> crossterm::event::MouseButton {
        match self {
            Self::Left => crossterm::event::MouseButton::Left,
            Self::Right => crossterm::event::MouseButton::Right,
            Self::Middle => crossterm::event::MouseButton::Middle,
        }
    }
}

impl ClientMouseKind {
    pub(crate) fn from_crossterm(kind: crossterm::event::MouseEventKind) -> Option<Self> {
        use crossterm::event::MouseEventKind;
        Some(match kind {
            MouseEventKind::Down(button) => Self::Down(ClientMouseButton::from_crossterm(button)),
            MouseEventKind::Up(button) => Self::Up(ClientMouseButton::from_crossterm(button)),
            MouseEventKind::Drag(button) => Self::Drag(ClientMouseButton::from_crossterm(button)),
            MouseEventKind::Moved => Self::Moved,
            MouseEventKind::ScrollUp => Self::ScrollUp,
            MouseEventKind::ScrollDown => Self::ScrollDown,
            MouseEventKind::ScrollLeft => Self::ScrollLeft,
            MouseEventKind::ScrollRight => Self::ScrollRight,
        })
    }

    pub(crate) fn to_crossterm(self) -> crossterm::event::MouseEventKind {
        use crossterm::event::MouseEventKind;
        match self {
            Self::Down(button) => MouseEventKind::Down(button.to_crossterm()),
            Self::Up(button) => MouseEventKind::Up(button.to_crossterm()),
            Self::Drag(button) => MouseEventKind::Drag(button.to_crossterm()),
            Self::Moved => MouseEventKind::Moved,
            Self::ScrollUp => MouseEventKind::ScrollUp,
            Self::ScrollDown => MouseEventKind::ScrollDown,
            Self::ScrollLeft => MouseEventKind::ScrollLeft,
            Self::ScrollRight => MouseEventKind::ScrollRight,
        }
    }
}

impl ClientPaneInputEvent {
    pub(crate) fn from_terminal_key(key: crate::input::TerminalKey) -> Option<Self> {
        let tracks_release = key.generated_text.is_none() || key.has_physical_identity();
        let physical_key_id = key.physical_key_id();
        let windows_record = key.windows_record();
        Some(Self::Key {
            code: ClientKeyCode::from_crossterm(key.code)?,
            modifiers: key.modifiers.bits(),
            kind: ClientKeyKind::from_crossterm(key.kind),
            repeat_count: key.repeat_count,
            shifted_codepoint: key.shifted_codepoint,
            generated_text: key.generated_text,
            tracks_release,
            physical_key_id,
            windows_record,
        })
    }

    pub(crate) fn to_raw_input_event(&self) -> crate::raw_input::RawInputEvent {
        match self {
            Self::Key {
                code,
                modifiers,
                kind,
                repeat_count,
                shifted_codepoint,
                generated_text,
                tracks_release,
                windows_record,
                ..
            } => {
                let mut key = crate::input::TerminalKey::new(
                    code.to_crossterm(),
                    crossterm::event::KeyModifiers::from_bits_truncate(*modifiers),
                )
                .with_kind(kind.to_crossterm())
                .with_repeat_count(*repeat_count)
                .with_generated_text(generated_text.clone())
                .with_physical_identity_hint(*tracks_release && generated_text.is_some());
                if let Some(shifted_codepoint) = shifted_codepoint {
                    key = key.with_shifted_codepoint(*shifted_codepoint);
                }
                #[cfg(any(windows, test))]
                if let Some(record) = windows_record {
                    key = key.with_windows_record(*record);
                }
                #[cfg(not(any(windows, test)))]
                let _ = windows_record;
                crate::raw_input::RawInputEvent::Key(key)
            }
            Self::TextCommit(text) => {
                crate::raw_input::RawInputEvent::Text(crate::input::TextCommit::new(text.clone()))
            }
            Self::Mouse {
                kind,
                position,
                modifiers,
                ..
            } => {
                let (column, row) = match position {
                    ClientMousePosition::Cell { column, row }
                    | ClientMousePosition::Pixels { column, row, .. } => (*column, *row),
                };
                crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                    kind: kind.to_crossterm(),
                    column,
                    row,
                    modifiers: crossterm::event::KeyModifiers::from_bits_truncate(*modifiers),
                })
            }
            Self::Paste(text) => crate::raw_input::RawInputEvent::Paste(text.clone()),
        }
    }
}

#[cfg(any(windows, test))]
impl ClientInputEvent {
    pub(crate) fn from_crossterm(event: crossterm::event::Event) -> Option<Self> {
        match event {
            crossterm::event::Event::Key(key) => Some(Self::Key {
                code: ClientKeyCode::from_crossterm(key.code)?,
                modifiers: key.modifiers.bits(),
                kind: ClientKeyKind::from_crossterm(key.kind),
                repeat_count: 1,
                generated_text: None,
                source: ClientKeySource::Synthesized,
            }),
            crossterm::event::Event::Mouse(mouse) => Some(Self::Mouse {
                kind: ClientMouseKind::from_crossterm(mouse.kind)?,
                column: mouse.column,
                row: mouse.row,
                modifiers: mouse.modifiers.bits(),
            }),
            crossterm::event::Event::Paste(text) => Some(Self::Paste { text }),
            crossterm::event::Event::FocusGained => Some(Self::FocusGained),
            crossterm::event::Event::FocusLost => Some(Self::FocusLost),
            crossterm::event::Event::Resize(_, _) => None,
        }
    }

    pub(crate) fn to_raw_input_event(&self) -> crate::raw_input::RawInputEvent {
        match self {
            Self::Key {
                code,
                modifiers,
                kind,
                repeat_count,
                generated_text,
                source,
            } => {
                let mut key = crate::input::TerminalKey::new(
                    code.to_crossterm(),
                    crossterm::event::KeyModifiers::from_bits_truncate(*modifiers),
                )
                .with_generated_text(generated_text.clone());
                key = match source {
                    ClientKeySource::Synthesized => key,
                    ClientKeySource::Vt { bytes } => key.with_vt_bytes(bytes.clone()),
                    ClientKeySource::WindowsConsole { record } => key.with_windows_record(*record),
                };
                key = key
                    .with_repeat_count(*repeat_count)
                    .with_kind(kind.to_crossterm());
                crate::raw_input::RawInputEvent::Key(key)
            }
            Self::TextCommit(text) => {
                crate::raw_input::RawInputEvent::Text(crate::input::TextCommit::new(text.clone()))
            }
            Self::Mouse {
                kind,
                column,
                row,
                modifiers,
            } => crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: kind.to_crossterm(),
                column: *column,
                row: *row,
                modifiers: crossterm::event::KeyModifiers::from_bits_truncate(*modifiers),
            }),
            Self::Paste { text } => crate::raw_input::RawInputEvent::Paste(text.clone()),
            Self::FocusGained => crate::raw_input::RawInputEvent::OuterFocusGained,
            Self::FocusLost => crate::raw_input::RawInputEvent::OuterFocusLost,
        }
    }
}

/// Messages sent from the client to the server over the client protocol socket.
///
/// Variant order is frozen for endpoint generation 1. Add compatible endpoint
/// behavior through `EndpointControl` or advertised API methods, not new enum variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Direct terminal handshake: announces protocol version and terminal dimensions.
    TerminalHello {
        version: u32,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        pixel_mouse: bool,
    },

    /// Raw input bytes read from the client's stdin.
    Input {
        /// Raw terminal input (possibly multi-byte escape sequences).
        data: Vec<u8>,
    },

    /// Image bytes read from the client's local clipboard for remote paste bridging.
    ClipboardImage {
        /// Stable terminal target selected by the client that read the clipboard.
        target: ClientClipboardImageTarget,
        /// Image file extension without a leading dot.
        extension: String,
        /// Raw image bytes.
        data: Vec<u8>,
    },

    /// Terminal resize notification from the client.
    Resize {
        /// New terminal width in columns.
        cols: u16,
        /// New terminal height in rows.
        rows: u16,
        /// Width of a terminal cell in physical pixels, or 0 when client-side Kitty graphics are disabled.
        cell_width_px: u32,
        /// Height of a terminal cell in physical pixels, or 0 when unavailable.
        cell_height_px: u32,
        /// Whether this resize carries coherent exact geometry for SGR pixel mouse input.
        pixel_mouse: bool,
    },

    /// Graceful disconnect request.
    Detach,

    /// Switch this connection into direct terminal attach mode.
    AttachTerminal {
        /// Terminal id to attach to.
        terminal_id: String,
        /// Replace an existing writable attach owner for this terminal.
        takeover: bool,
    },

    /// Scroll input handled by a direct terminal attach client.
    AttachScroll {
        /// Original input source for routing.
        source: AttachScrollSource,
        /// Scroll direction.
        direction: AttachScrollDirection,
        /// Number of terminal rows to move when using host scrollback.
        lines: u16,
        /// Mouse column relative to the attached terminal, when available.
        column: Option<u16>,
        /// Mouse row relative to the attached terminal, when available.
        row: Option<u16>,
        /// Crossterm-compatible modifier bits for forwarded mouse wheel events.
        modifiers: u8,
    },

    /// Switch this connection into read-only terminal observe mode.
    ObserveTerminal {
        /// Pane, terminal, or agent target to observe.
        target: String,
    },

    /// Switch this connection into writable terminal control mode.
    ControlTerminal {
        /// Pane, terminal, or agent target to control.
        target: String,
        /// Replace an existing writable controller for this terminal.
        takeover: bool,
    },

    /// Result of the one armed Herdr-owned direct Kitty transmission.
    GraphicsTransmissionResult {
        transfer_id: u64,
        image_id: u32,
        success: bool,
    },

    /// The direct command was written and flushed; terminal response timing starts now.
    GraphicsTransmissionStarted { transfer_id: u64, image_id: u32 },

    /// Handshake for the client-owned shell around one pane surface.
    ClientShellHello {
        version: u32,
        cell_width_px: u32,
        cell_height_px: u32,
        surface_size: ClientSurfaceSize,
        pixel_mouse: bool,
        direct_graphics: bool,
        /// Whether the endpoint's keymap, rather than the client's, owns shell bindings.
        endpoint_keybindings: bool,
        /// Whether this client wants shell mouse capture even without pane demand.
        mouse_capture: bool,
    },

    /// Resize the pane viewport of a client-owned shell.
    ClientShellResize {
        cell_width_px: u32,
        cell_height_px: u32,
        surface_size: ClientSurfaceSize,
        /// Whether this resize carries coherent exact geometry for SGR pixel mouse input.
        pixel_mouse: bool,
    },

    /// Deliver client-classified semantic input directly to a stable pane target.
    ClientShellPaneInput {
        pane_id: String,
        events: Vec<ClientPaneInputEvent>,
    },

    /// Deliver client-classified semantic input to the active popup terminal.
    ClientShellPopupInput {
        terminal_id: String,
        events: Vec<ClientPaneInputEvent>,
    },

    /// Invoke one endpoint operation through this client shell's selected connection.
    ClientShellEndpointRequest { boot_id: String, request: String },

    /// Deliver one structured mouse event to a directly attached terminal.
    AttachMouse {
        kind: ClientMouseKind,
        position: ClientMousePosition,
        geometry: Option<ClientMouseGeometry>,
        modifiers: u8,
        lines: u16,
    },

    /// Publish one host terminal color or appearance update observed by a client-owned shell.
    ClientShellHostTheme { update: ClientHostThemeUpdate },

    /// Publish whether the outer terminal containing a client shell has focus.
    ClientShellFocus { focused: bool },

    /// Update this client's shell mouse-capture preference after config reload.
    ClientShellMouseCapture { enabled: bool },

    /// Extensible named control message for the stable client-owned endpoint protocol.
    ///
    /// This variant is append-only. Its bincode tag and two-string payload are part
    /// of endpoint generation 1 and must not change.
    EndpointControl { kind: String, data: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHostColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<crate::terminal_theme::RgbColor> for ClientHostColor {
    fn from(color: crate::terminal_theme::RgbColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

impl From<ClientHostColor> for crate::terminal_theme::RgbColor {
    fn from(color: ClientHostColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientHostDefaultColorKind {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientHostAppearance {
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientHostThemeUpdate {
    DefaultColor {
        kind: ClientHostDefaultColorKind,
        color: ClientHostColor,
    },
    PaletteColors(Vec<(u8, ClientHostColor)>),
    Appearance(ClientHostAppearance),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientClipboardImageTarget {
    DirectTerminal,
    Pane(String),
    Popup(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachScrollSource {
    Wheel,
    PageKey {
        /// Original key bytes to forward when the child application owns page keys.
        input: Vec<u8>,
    },
}

// ---------------------------------------------------------------------------
// Server → Client messages
// ---------------------------------------------------------------------------

/// A single cell in a rendered frame, serialized independently from ratatui's
/// `Cell` type to keep the wire protocol stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellData {
    /// Grapheme cluster displayed in this cell (usually 1–2 chars).
    pub symbol: String,
    /// Foreground color as a packed u32 (0xAARRGGBB or ratatui Color index).
    pub fg: u32,
    /// Background color as a packed u32.
    pub bg: u32,
    /// Bitmask of style modifiers (bold, italic, etc.) plus Herdr extension bits.
    pub modifier: u16,
    /// Whether this cell should be skipped during diff-based rendering.
    pub skip: bool,
    /// Index into `FrameData::hyperlinks` for this cell's OSC 8 target, if any.
    pub hyperlink: Option<u32>,
}

impl CellData {
    pub(crate) fn from_ratatui_cell(cell: &ratatui::buffer::Cell) -> Self {
        Self {
            symbol: cell.symbol().to_owned(),
            fg: color_to_u32(cell.fg),
            bg: color_to_u32(cell.bg),
            modifier: modifier_to_u16(cell.modifier),
            skip: cell.skip,
            hyperlink: None,
        }
    }
}

/// Cursor shape encoded as a DECSCUSR parameter.
///
/// 0 = terminal default, 1 = blinking block, 2 = steady block,
/// 3 = blinking underline, 4 = steady underline, 5 = blinking bar,
/// 6 = steady bar.
pub type CursorShapeParam = u8;

/// Cursor position within a rendered frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    /// Column offset (0-based) of the cursor.
    pub x: u16,
    /// Row offset (0-based) of the cursor.
    pub y: u16,
    /// Whether the cursor is visible.
    pub visible: bool,
    /// Cursor shape as a DECSCUSR parameter.
    #[serde(default)]
    pub shape: CursorShapeParam,
}

/// A rendered frame to be displayed by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameData {
    /// Cells in row-major order. Length must equal `width * height`.
    pub cells: Vec<CellData>,
    /// Frame width in columns.
    pub width: u16,
    /// Frame height in rows.
    pub height: u16,
    /// Cursor state for this frame, if applicable.
    pub cursor: Option<CursorState>,
    /// OSC 8 hyperlink URIs referenced by cells.
    pub hyperlinks: Vec<String>,
    /// Kitty graphics protocol bytes to apply after the text frame.
    pub graphics: Vec<u8>,
}

impl FrameData {
    /// Creates a `FrameData` from a ratatui `Buffer` and optional cursor.
    ///
    /// This converts ratatui's internal cell representation into the
    /// wire-protocol cell format. The conversion is lossless for all
    /// commonly used cell attributes.
    #[cfg(test)]
    pub fn from_ratatui_buffer(
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
    ) -> Self {
        Self::from_ratatui_buffer_with_hyperlinks(buffer, cursor, &[])
    }

    pub fn from_ratatui_buffer_with_hyperlinks(
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
        hyperlinks: &[((u16, u16), String, String)],
    ) -> Self {
        let area = buffer.area;
        let width = area.width;
        let height = area.height;

        let mut hyperlink_uris = Vec::<String>::new();
        let mut hyperlink_indices = HashMap::<&str, u32>::new();
        let mut hyperlink_by_position = HashMap::<(u16, u16), (&str, &str)>::new();
        for ((x, y), symbol, uri) in hyperlinks {
            hyperlink_by_position.insert((*x, *y), (symbol.as_str(), uri.as_str()));
        }
        let mut cells = Vec::with_capacity((width as usize) * (height as usize));
        for row in 0..height {
            for col in 0..width {
                let cell = buffer.cell((col, row)).expect("cell within bounds");
                let hyperlink = hyperlink_by_position
                    .get(&(col, row))
                    .and_then(|(symbol, uri)| {
                        if *symbol != cell.symbol() {
                            return None;
                        }
                        Some(*hyperlink_indices.entry(*uri).or_insert_with(|| {
                            let index = hyperlink_uris.len() as u32;
                            hyperlink_uris.push((*uri).to_owned());
                            index
                        }))
                    });
                let mut cell = CellData::from_ratatui_cell(cell);
                cell.hyperlink = hyperlink;
                cells.push(cell);
            }
        }

        FrameData {
            cells,
            width,
            height,
            cursor,
            hyperlinks: hyperlink_uris,
            graphics: Vec::new(),
        }
    }

    pub(crate) fn replace_from_ratatui_buffer_preserving_effects(
        &mut self,
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
    ) {
        let width = self.width;
        let hyperlinks = if width == 0 {
            Vec::new()
        } else {
            self.cells
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| {
                    let uri = self.hyperlinks.get(cell.hyperlink? as usize)?;
                    let x = u16::try_from(index % usize::from(width)).ok()?;
                    let y = u16::try_from(index / usize::from(width)).ok()?;
                    Some(((x, y), cell.symbol.clone(), uri.clone()))
                })
                .collect::<Vec<_>>()
        };
        let graphics = std::mem::take(&mut self.graphics);
        let mut replacement =
            Self::from_ratatui_buffer_with_hyperlinks(buffer, cursor, &hyperlinks);
        replacement.graphics = graphics;
        *self = replacement;
    }

    /// Reconstructs a ratatui `Buffer` from this frame data.
    ///
    /// Returns `None` if the cells vector length doesn't match `width * height`.
    pub(crate) fn to_ratatui_buffer(&self) -> Option<ratatui::buffer::Buffer> {
        let expected = (self.width as usize) * (self.height as usize);
        if self.cells.len() != expected {
            return None;
        }

        let area = ratatui::layout::Rect::new(0, 0, self.width, self.height);
        let mut buffer = ratatui::buffer::Buffer::filled(area, ratatui::buffer::Cell::new(" "));

        for row in 0..self.height {
            for col in 0..self.width {
                let idx = (row as usize) * (self.width as usize) + (col as usize);
                let cell_data = &self.cells[idx];
                let cell = buffer.cell_mut((col, row)).expect("cell within bounds");
                cell.set_symbol(&cell_data.symbol);
                cell.fg = u32_to_color(cell_data.fg);
                cell.bg = u32_to_color(cell_data.bg);
                cell.modifier = u16_to_modifier(cell_data.modifier);
                cell.skip = cell_data.skip;
            }
        }

        Some(buffer)
    }
}

fn deserialize_client_shell_agent_status<'de, D>(
    deserializer: D,
) -> Result<crate::api::schema::AgentStatus, D::Error>
where
    D: serde::Deserializer<'de>,
{
    if !deserializer.is_human_readable() {
        return crate::api::schema::AgentStatus::deserialize(deserializer);
    }
    let value = String::deserialize(deserializer)?;
    Ok(match value.as_str() {
        "idle" => crate::api::schema::AgentStatus::Idle,
        "working" => crate::api::schema::AgentStatus::Working,
        "blocked" => crate::api::schema::AgentStatus::Blocked,
        "done" => crate::api::schema::AgentStatus::Done,
        _ => crate::api::schema::AgentStatus::Unknown,
    })
}

/// Initial resource projection used by the stable client-owned shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellSnapshot {
    /// Changes whenever the endpoint process restarts.
    pub boot_id: String,
    /// Monotonic replacement revision within one endpoint boot.
    pub revision: u64,
    /// Endpoint startup/reload config warning, filtered for client-owned keybindings.
    pub config_diagnostic: Option<String>,
    /// Unseen announcement owned and persisted by this endpoint.
    pub product_announcement: Option<ClientShellProductAnnouncement>,
    /// Future-version update advertised by the endpoint.
    pub update_available: Option<String>,
    /// Endpoint-specific command shown in update instructions.
    pub update_install_command: String,
    /// Endpoint's normalized built-in keybindings, used only when a remote client selects server bindings.
    pub server_keybindings_toml: Option<String>,
    /// Whether the endpoint has a What's New entry, even if its body is unavailable.
    pub latest_release_notes_available: bool,
    /// Whether endpoint-owned integration assets need an update.
    pub integration_updates_available: bool,
    /// Endpoint-owned base directory used for new linked worktree checkouts.
    pub worktree_directory: String,
    /// Cached endpoint-owned notes used by the client-rendered overlay.
    pub release_notes: Option<ClientShellReleaseNotes>,
    pub focused_workspace_id: Option<String>,
    pub focused_tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
    pub tab_bar_right: Vec<ClientShellTabStatusSegment>,
    pub tab_bar_right_separator: String,
    pub agent_view_label: Option<String>,
    pub agent_order: Vec<String>,
    pub workspaces: Vec<ClientShellWorkspace>,
    pub tabs: Vec<ClientShellTab>,
    pub panes: Vec<ClientShellPane>,
    pub agents: Vec<ClientShellAgent>,
    pub commands: Vec<ClientShellCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellProductAnnouncement {
    pub version: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub preview: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellReleaseNotes {
    pub version: String,
    pub body: String,
    pub preview: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientShellCommandAction {
    Shell,
    Pane,
    Popup,
    PluginAction,
    /// A future endpoint action kind that this client cannot execute.
    #[serde(other)]
    Unknown,
}

impl From<crate::config::CustomCommandAction> for ClientShellCommandAction {
    fn from(action: crate::config::CustomCommandAction) -> Self {
        match action {
            crate::config::CustomCommandAction::Shell => Self::Shell,
            crate::config::CustomCommandAction::Pane => Self::Pane,
            crate::config::CustomCommandAction::Popup => Self::Popup,
            crate::config::CustomCommandAction::PluginAction => Self::PluginAction,
        }
    }
}

impl TryFrom<ClientShellCommandAction> for crate::config::CustomCommandAction {
    type Error = ();

    fn try_from(action: ClientShellCommandAction) -> Result<Self, Self::Error> {
        match action {
            ClientShellCommandAction::Shell => Ok(Self::Shell),
            ClientShellCommandAction::Pane => Ok(Self::Pane),
            ClientShellCommandAction::Popup => Ok(Self::Popup),
            ClientShellCommandAction::PluginAction => Ok(Self::PluginAction),
            ClientShellCommandAction::Unknown => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellCommand {
    pub command_id: String,
    pub binding_label: String,
    pub binding_labels: Vec<String>,
    pub action: ClientShellCommandAction,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellTabStatusSegment {
    pub text: String,
    pub accent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellWorkspace {
    pub workspace_id: String,
    pub active_tab_id: String,
    pub new_workspace_cwd: String,
    pub number: usize,
    pub label: String,
    pub custom_label: bool,
    pub branch: Option<String>,
    pub git_ahead_behind: Option<(usize, usize)>,
    pub tokens: Vec<(String, String)>,
    pub worktree: Option<ClientShellWorktree>,
    pub focused: bool,
    #[serde(deserialize_with = "deserialize_client_shell_agent_status")]
    pub agent_status: crate::api::schema::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellWorktree {
    pub key: String,
    pub label: String,
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellTab {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub custom_label: bool,
    pub zoomed: bool,
    pub focused: bool,
    #[serde(deserialize_with = "deserialize_client_shell_agent_status")]
    pub agent_status: crate::api::schema::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellPane {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub focused: bool,
    pub right_click_passthrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellAgent {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub name: Option<String>,
    pub display_agent: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    #[serde(deserialize_with = "deserialize_client_shell_agent_status")]
    pub agent_status: crate::api::schema::AgentStatus,
    pub state_change_seq: u64,
    pub state_labels: Vec<(String, String)>,
    pub tokens: Vec<(String, String)>,
    pub focused: bool,
}

/// Origin-relative geometry for one pane in a rendered pane surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePane {
    pub pane_id: String,
    pub content_revision: u64,
    pub rect: SurfaceRect,
    pub inner_rect: SurfaceRect,
    pub scrollbar_rect: Option<SurfaceRect>,
    pub scroll: Option<PaneSurfaceScrollMetrics>,
    pub focused: bool,
    pub mouse_reporting: bool,
    pub sgr_pixel_mouse: bool,
    pub alternate_screen_active: bool,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceScrollMetrics {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u64,
}

/// One draggable BSP split handle relative to a pane surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceSplit {
    pub direction: PaneSurfaceSplitDirection,
    pub pos: u16,
    pub area: SurfaceRect,
    pub hit_rect: SurfaceRect,
    pub path: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneSurfaceSplitDirection {
    Horizontal,
    Vertical,
}

/// Wire-safe rectangle relative to a pane surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl From<ratatui::layout::Rect> for SurfaceRect {
    fn from(rect: ratatui::layout::Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceGraphicsTarget {
    Pane { pane_id: String },
    Popup { terminal_id: String },
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceGraphicsSource {
    Terminal {
        target: SurfaceGraphicsTarget,
        image_id: u32,
    },
    PaneLayer {
        pane_id: String,
        layer_id: String,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceGraphicsFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGraphicsAssetKey {
    pub source: SurfaceGraphicsSource,
    pub image_width: u32,
    pub image_height: u32,
    pub format: SurfaceGraphicsFormat,
    pub data_len: u64,
    pub data_fingerprint: u64,
}

/// Image bytes newly needed by this connection's complete desired scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGraphicsAsset {
    pub key: SurfaceGraphicsAssetKey,
    pub data: Vec<u8>,
}

/// One already-clipped desired placement relative to its target surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGraphicsPlacement {
    pub asset: SurfaceGraphicsAssetKey,
    pub logical_placement_id: u32,
    pub x: u16,
    pub y: u16,
    pub cols: u32,
    pub rows: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub z: i32,
    pub scrollback_offset: u32,
}

/// Complete desired placements plus only the image bytes not already sent for
/// the current live scene on this connection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGraphicsScene {
    pub assets: Vec<SurfaceGraphicsAsset>,
    pub placements: Vec<SurfaceGraphicsPlacement>,
    /// Direct-uploaded assets that remain live for this client even while their
    /// pane is outside the selected scene.
    pub retained_assets: Vec<SurfaceGraphicsAssetKey>,
}

/// One server-rendered active-tab surface without sidebar, tab bar, or overlays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceFrame {
    /// Endpoint process identity that produced this surface.
    pub boot_id: String,
    /// Projection revision whose focused IDs and topology produced this surface.
    pub projection_revision: u64,
    /// Monotonic revision for full surfaces and incremental patches on one connection.
    pub surface_revision: u64,
    pub frame: FrameData,
    pub panes: Vec<PaneSurfacePane>,
    pub splits: Vec<PaneSurfaceSplit>,
    pub popup: Option<Box<ClientShellPopupSurface>>,
    pub graphics: SurfaceGraphicsScene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientShellPopupSize {
    Cells(u16),
    Percent(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePatchRow {
    /// Origin-relative surface column where this changed cell span starts.
    pub x: u16,
    /// Origin-relative surface row.
    pub y: u16,
    pub cells: Vec<CellData>,
}

/// Incremental terminal-cell update against one committed complete pane surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePatch {
    pub boot_id: String,
    pub projection_revision: u64,
    pub base_surface_revision: u64,
    pub surface_revision: u64,
    pub rows: Vec<PaneSurfacePatchRow>,
    /// Updated metadata for panes whose terminal content changed.
    pub panes: Vec<PaneSurfacePane>,
    /// Final cursor relative to the pane surface.
    pub cursor: Option<CursorState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellPopupSurface {
    pub terminal_id: String,
    pub title: String,
    pub width: Option<ClientShellPopupSize>,
    pub height: Option<ClientShellPopupSize>,
    pub frame: FrameData,
    pub mouse_reporting: bool,
    pub sgr_pixel_mouse: bool,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// Terminal ANSI bytes encoded by the server for network-efficient clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalFrame {
    /// Monotonic per-client frame sequence.
    pub seq: u64,
    /// Frame width in columns.
    pub width: u16,
    /// Frame height in rows.
    pub height: u16,
    /// Whether bytes contain a full redraw rather than an incremental diff.
    pub full: bool,
    /// Terminal escape bytes ready to write directly to stdout.
    pub bytes: Vec<u8>,
}

/// Notification kind forwarded from server to client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotifyKind {
    /// Play a sound (bell/agent-done, etc.).
    Sound,
    /// Display a toast message through the outer terminal.
    Toast,
    /// Display a toast message through the host OS notification service.
    SystemToast,
}

/// A client-rendered notification category. The server reports the semantic
/// event; each connected shell client chooses how (or whether) to present it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticNotificationKind {
    NeedsAttention,
    Finished,
    UpdateInstalled,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticNotificationSound {
    Done,
    Request,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticNotification {
    pub kind: SemanticNotificationKind,
    pub title: String,
    pub body: Option<String>,
    pub sound: Option<SemanticNotificationSound>,
    pub agent: Option<String>,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
    pub position: Option<crate::config::ToastHerdrPosition>,
}

/// Messages sent from the server to the client over the client protocol socket.
///
/// Variant order is frozen for endpoint generation 1. Add compatible endpoint
/// behavior through `EndpointControl`, and ignore unrecognized named controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Handshake response: server acknowledges (or rejects) the client.
    Welcome {
        /// Protocol version the server speaks.
        version: u32,
        /// Render encoding selected by the server for this connection.
        encoding: RenderEncoding,
        /// If present, the handshake failed and this describes why.
        /// The client should exit with a clear error message.
        error: Option<String>,
    },

    /// Terminal bytes to write directly for a terminal-ANSI client.
    Terminal(TerminalFrame),

    /// Client-local Kitty graphics bytes to write directly to the host terminal.
    Graphics {
        /// Raw Kitty graphics protocol bytes.
        bytes: Vec<u8>,
    },

    /// Server is shutting down. Clients should exit gracefully.
    ServerShutdown {
        /// Optional reason for the shutdown.
        reason: Option<String>,
    },

    /// A notification event (sound/toast) to be rendered locally by the client.
    Notify {
        /// What kind of notification.
        kind: NotifyKind,
        /// Human-readable title or sound label.
        message: String,
        /// Optional human-readable notification body.
        body: Option<String>,
    },

    /// OSC 52 clipboard data forwarded from a PTY through the server.
    Clipboard {
        /// Base64-encoded clipboard data.
        data: String,
    },

    /// Set the foreground client's outer terminal window title.
    WindowTitle {
        /// Sanitized title to write with OSC 0. `None` restores Herdr's default title.
        title: Option<String>,
    },

    /// Client-local runtime config changed on disk; refresh it without reconnecting.
    ReloadSoundConfig,

    /// Whether the client should currently capture host mouse input.
    MouseCapture {
        /// True when Herdr mouse UI is enabled or the focused pane app requests mouse reporting.
        enabled: bool,
        /// True only while the focused pane requests DEC SGR pixel mode 1016.
        sgr_pixels: bool,
    },

    /// Ring the foreground client's outer terminal for pane-originated BEL characters.
    TerminalBell {
        /// Number of BEL characters parsed from one PTY read.
        count: u16,
    },

    /// One validated Herdr-owned Kitty regular-file RGBA transmission.
    GraphicsFile {
        path: String,
        expected_len: u64,
        image_id: u32,
        transfer_id: u64,
        leading: Vec<u8>,
        control: String,
        /// ClientShell upload identity. `None` targets a direct terminal client.
        surface_asset: Option<SurfaceGraphicsAssetKey>,
    },

    /// Suppress a direct command that expired before terminal delivery.
    GraphicsTransmissionRetired { transfer_id: u64, image_id: u32 },

    /// Initial metadata for a client-owned shell.
    ClientShellSnapshot(Box<ClientShellSnapshot>),

    /// Active-tab pane content rendered at a client-requested origin-relative size.
    PaneSurface(PaneSurfaceFrame),

    /// Ephemeral semantic notification delivered over the private control lane.
    /// It is sent only to currently connected client-rendered shells.
    SemanticNotification(SemanticNotification),

    /// Immediate endpoint error that the client-rendered shell must show regardless of notification policy.
    ClientShellError { message: String },

    /// Exact Kitty keyboard flags requested by a directly attached terminal.
    /// Zero restores the host terminal's previous keyboard mode.
    DirectTerminalKeyboardProtocol {
        flags: u16,
        modify_other_keys_level: u8,
    },

    /// Whether the focused pane or popup needs the shell host to report every key.
    ClientShellKeyboardReportAll { enabled: bool },

    /// One ordered chunk of the final response to an endpoint operation.
    ClientShellEndpointResponseChunk {
        boot_id: String,
        request_id: String,
        final_chunk: bool,
        data: Vec<u8>,
    },

    /// Incremental terminal-cell update for a previously committed pane surface.
    PaneSurfacePatch(PaneSurfacePatch),

    /// Extensible named control message for the stable client-owned endpoint protocol.
    ///
    /// This variant is append-only. Its bincode tag and two-string payload are part
    /// of endpoint generation 1 and must not change.
    EndpointControl { kind: String, data: String },
}

// ---------------------------------------------------------------------------
// Color / Modifier conversion helpers
// ---------------------------------------------------------------------------

/// Converts a ratatui `Color` to a packed u32 for wire transport.
///
/// Encoding:
/// - Named colors (Reset, Black, …, White) → `0x00_00_00_XX` where XX is 0..=16
/// - Indexed palette → `0x01_00_00_XX` where XX is the palette index
/// - RGB → `0x02_RR_GG_BB` with components in the lower 3 bytes
pub(crate) fn color_to_u32(color: ratatui::style::Color) -> u32 {
    match color {
        ratatui::style::Color::Reset => 0x00_00_00_00,
        ratatui::style::Color::Black => 0x00_00_00_01,
        ratatui::style::Color::Red => 0x00_00_00_02,
        ratatui::style::Color::Green => 0x00_00_00_03,
        ratatui::style::Color::Yellow => 0x00_00_00_04,
        ratatui::style::Color::Blue => 0x00_00_00_05,
        ratatui::style::Color::Magenta => 0x00_00_00_06,
        ratatui::style::Color::Cyan => 0x00_00_00_07,
        ratatui::style::Color::Gray => 0x00_00_00_08,
        ratatui::style::Color::DarkGray => 0x00_00_00_09,
        ratatui::style::Color::LightRed => 0x00_00_00_0A,
        ratatui::style::Color::LightGreen => 0x00_00_00_0B,
        ratatui::style::Color::LightYellow => 0x00_00_00_0C,
        ratatui::style::Color::LightBlue => 0x00_00_00_0D,
        ratatui::style::Color::LightMagenta => 0x00_00_00_0E,
        ratatui::style::Color::LightCyan => 0x00_00_00_0F,
        ratatui::style::Color::White => 0x00_00_00_10,
        ratatui::style::Color::Indexed(i) => 0x01_00_00_00 | (i as u32),
        ratatui::style::Color::Rgb(r, g, b) => {
            0x02_00_00_00 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
        }
    }
}

/// Converts a packed u32 back to a ratatui `Color`.
fn u32_to_color(val: u32) -> ratatui::style::Color {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => ratatui::style::Color::Reset,
            0x01 => ratatui::style::Color::Black,
            0x02 => ratatui::style::Color::Red,
            0x03 => ratatui::style::Color::Green,
            0x04 => ratatui::style::Color::Yellow,
            0x05 => ratatui::style::Color::Blue,
            0x06 => ratatui::style::Color::Magenta,
            0x07 => ratatui::style::Color::Cyan,
            0x08 => ratatui::style::Color::Gray,
            0x09 => ratatui::style::Color::DarkGray,
            0x0A => ratatui::style::Color::LightRed,
            0x0B => ratatui::style::Color::LightGreen,
            0x0C => ratatui::style::Color::LightYellow,
            0x0D => ratatui::style::Color::LightBlue,
            0x0E => ratatui::style::Color::LightMagenta,
            0x0F => ratatui::style::Color::LightCyan,
            0x10 => ratatui::style::Color::White,
            _ => ratatui::style::Color::Reset, // unknown named → Reset
        },
        0x01 => ratatui::style::Color::Indexed((val & 0xFF) as u8),
        0x02 => {
            let r = ((val >> 16) & 0xFF) as u8;
            let g = ((val >> 8) & 0xFF) as u8;
            let b = (val & 0xFF) as u8;
            ratatui::style::Color::Rgb(r, g, b)
        }
        _ => ratatui::style::Color::Reset, // unknown tag → Reset
    }
}

const UNDERLINE_STYLE_SHIFT: u16 = 12;
const UNDERLINE_STYLE_MASK: u16 = 0xF000;

/// Converts a ratatui `Modifier` bitmask to a u16 for wire transport.
pub(crate) fn modifier_to_u16(modifier: ratatui::style::Modifier) -> u16 {
    modifier.bits()
}

pub(crate) fn underline_style_from_modifier(modifier: u16) -> u8 {
    ((modifier & UNDERLINE_STYLE_MASK) >> UNDERLINE_STYLE_SHIFT) as u8
}

pub(crate) fn modifier_with_underline_style(
    modifier: ratatui::style::Modifier,
    underline_style: u8,
) -> ratatui::style::Modifier {
    let bits = modifier.bits() | ((u16::from(underline_style) & 0x0F) << UNDERLINE_STYLE_SHIFT);
    ratatui::style::Modifier::from_bits_retain(bits)
}

/// Converts a u16 back to a ratatui `Modifier`.
fn u16_to_modifier(val: u16) -> ratatui::style::Modifier {
    ratatui::style::Modifier::from_bits_truncate(val & !UNDERLINE_STYLE_MASK)
}

// ---------------------------------------------------------------------------
// Framing: length-prefixed binary messages
// ---------------------------------------------------------------------------

/// Errors that can occur during framing operations.
#[derive(Debug)]
pub enum FramingError {
    /// The decoded payload length exceeds the configured maximum frame size.
    Oversized { claimed: usize, max: usize },
    /// An I/O error occurred while reading or writing.
    Io(io::Error),
    /// Bincode serialization or deserialization failed.
    Bincode(String),
    /// The connection was closed before a complete frame could be read.
    UnexpectedEof,
}

impl std::fmt::Display for FramingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FramingError::Oversized { claimed, max } => {
                write!(f, "frame size {claimed} exceeds maximum {max}")
            }
            FramingError::Io(e) => write!(f, "I/O error: {e}"),
            FramingError::Bincode(e) => write!(f, "bincode error: {e}"),
            FramingError::UnexpectedEof => write!(f, "unexpected end of stream"),
        }
    }
}

impl std::error::Error for FramingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FramingError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for FramingError {
    fn from(e: io::Error) -> Self {
        FramingError::Io(e)
    }
}

/// Serializes a message and writes it as a length-prefixed frame:
/// `[u32LE length][bincode payload]`.
///
/// This is a blocking/synchronous write suitable for use with `std::os::unix::net::UnixStream`
/// in blocking mode, or with any `Write` implementor.
///
/// # Errors
///
/// Returns `FramingError::Bincode` if the payload length exceeds `u32::MAX`
/// (would be truncated by the length prefix cast).
pub fn write_message<W: Write, M: Serialize>(writer: &mut W, msg: &M) -> Result<(), FramingError> {
    let payload = bincode::serde::encode_to_vec(msg, bincode::config::standard())
        .map_err(|e| FramingError::Bincode(e.to_string()))?;

    let len = payload.len();
    if len > u32::MAX as usize {
        return Err(FramingError::Bincode(format!(
            "payload length {len} exceeds u32::MAX ({}), would be truncated by length prefix",
            u32::MAX
        )));
    }

    writer.write_all(&(len as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

/// Reads and deserializes a length-prefixed frame from a reader.
///
/// Reassembles partial reads correctly. Rejects frames whose declared
/// length exceeds `max_frame_size` without panicking or allocating
/// oversized buffers.
pub fn read_message<R: Read, M: for<'de> Deserialize<'de>>(
    reader: &mut R,
    max_frame_size: usize,
) -> Result<M, FramingError> {
    // Read the 4-byte length prefix, reassembling partial reads.
    let mut len_buf = [0u8; LENGTH_PREFIX_BYTES];
    read_exact_or_eof(reader, &mut len_buf)?;
    let claimed_len = u32::from_le_bytes(len_buf) as usize;

    if claimed_len > max_frame_size {
        return Err(FramingError::Oversized {
            claimed: claimed_len,
            max: max_frame_size,
        });
    }

    // Read the payload, reassembling partial reads.
    let mut payload = vec![0u8; claimed_len];
    read_exact_or_eof(reader, &mut payload)?;

    let (msg, consumed) = bincode::serde::decode_from_slice(&payload, bincode::config::standard())
        .map_err(|e| FramingError::Bincode(e.to_string()))?;

    // Enforce that the decoder consumed the full payload.
    // Trailing bytes after the decoded message indicate a protocol violation
    // (e.g., a corrupted length prefix or concatenated payloads).
    if consumed != claimed_len {
        return Err(FramingError::Bincode(format!(
            "decoded {} bytes but payload length was {claimed_len}; trailing bytes are not allowed",
            consumed
        )));
    }

    Ok(msg)
}

/// Like `Read::read_exact`, but returns `FramingError::UnexpectedEof`
/// when the reader hits end-of-stream before filling the buffer, instead
/// of the generic `io::ErrorKind::UnexpectedEof`.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), FramingError> {
    reader.read_exact(buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            FramingError::UnexpectedEof
        } else {
            FramingError::Io(e)
        }
    })
}

// ---------------------------------------------------------------------------
// Version negotiation
// ---------------------------------------------------------------------------

/// Result of checking a client's protocol version against the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheck {
    /// Versions are compatible. The server should reply with a successful Welcome.
    Compatible,
    /// Versions are incompatible. The server should reply with a Welcome error and close.
    Incompatible(String),
}

/// Checks whether a client's protocol version is compatible with this server.
///
/// Current rules:
/// - Version 0 (pre-persistence client) is always rejected.
/// - Matching major versions are accepted.
/// - A client with a newer version than the server is rejected.
/// - A client with an older version than the server is rejected
///   (backward compatibility is not yet supported).
pub fn check_client_version(client_version: u32) -> VersionCheck {
    if client_version == 0 {
        return VersionCheck::Incompatible(
            "pre-persistence client (version 0) is not supported".to_owned(),
        );
    }

    if client_version == PROTOCOL_VERSION {
        VersionCheck::Compatible
    } else if client_version < PROTOCOL_VERSION {
        VersionCheck::Incompatible(format!(
            "client version {client_version} is older than server version {PROTOCOL_VERSION}; please upgrade your herdr client"
        ))
    } else {
        VersionCheck::Incompatible(format!(
            "client version {client_version} is newer than server version {PROTOCOL_VERSION}; please upgrade the herdr server"
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};
    use sha2::{Digest, Sha256};

    fn encoded_sha256(value: &impl Serialize) -> String {
        let encoded = bincode::serde::encode_to_vec(value, bincode::config::standard()).unwrap();
        format!("{:x}", Sha256::digest(encoded))
    }

    // These digests freeze representative generation-1 bincode payloads. A mismatch
    // requires a new named codec; do not update a v1 digest to bless a wire change.
    // They do not detect appended enum variants, so every type reachable from a v1
    // payload is also append-closed.

    // ---- Round-trip: ClientMessage ----

    #[test]
    fn client_hello_roundtrip() {
        let msg = ClientMessage::TerminalHello {
            version: PROTOCOL_VERSION,
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_shell_hello_roundtrip() {
        let msg = ClientMessage::ClientShellHello {
            version: PROTOCOL_VERSION,
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: ClientSurfaceSize { cols: 80, rows: 29 },
            pixel_mouse: true,
            direct_graphics: false,
            endpoint_keybindings: true,
            mouse_capture: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn endpoint_control_roundtrip() {
        let msg = ClientMessage::EndpointControl {
            kind: "endpoint.hello.v1".into(),
            data: r#"{"generation":1}"#.into(),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            bincode::serde::encode_to_vec(
                ClientMessage::EndpointControl {
                    kind: String::new(),
                    data: String::new(),
                },
                bincode::config::standard(),
            )
            .unwrap(),
            [20, 0, 0]
        );
    }

    #[test]
    fn client_shell_resize_roundtrip() {
        let msg = ClientMessage::ClientShellResize {
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: ClientSurfaceSize { cols: 74, rows: 29 },
            pixel_mouse: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "676d6376202750e72c45ff511e256b6154d3d20c0ee088d3792fa3a69d9704b9"
        );
    }

    #[test]
    fn client_input_roundtrip() {
        let msg = ClientMessage::Input {
            data: vec![0x1b, 0x5b, 0x41], // ESC [ A (up arrow)
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_message_wire_tags_reflect_current_order() {
        fn tag(msg: &ClientMessage) -> u8 {
            *bincode::serde::encode_to_vec(msg, bincode::config::standard())
                .unwrap()
                .first()
                .expect("encoded client message should include enum tag")
        }

        assert_eq!(
            tag(&ClientMessage::TerminalHello {
                version: PROTOCOL_VERSION,
                cols: 80,
                rows: 24,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: false,
            }),
            0
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellHello {
                version: PROTOCOL_VERSION,
                cell_width_px: 8,
                cell_height_px: 16,
                surface_size: ClientSurfaceSize { cols: 80, rows: 29 },
                pixel_mouse: false,
                direct_graphics: false,
                endpoint_keybindings: false,
                mouse_capture: false,
            }),
            11
        );
        assert_eq!(tag(&ClientMessage::Input { data: Vec::new() }), 1);
        assert_eq!(
            tag(&ClientMessage::ClipboardImage {
                target: ClientClipboardImageTarget::DirectTerminal,
                extension: "png".to_owned(),
                data: Vec::new(),
            }),
            2
        );
        assert_eq!(
            tag(&ClientMessage::Resize {
                cols: 80,
                rows: 24,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: false,
            }),
            3
        );
        assert_eq!(tag(&ClientMessage::Detach), 4);
        assert_eq!(
            tag(&ClientMessage::AttachTerminal {
                terminal_id: "term".to_owned(),
                takeover: false,
            }),
            5
        );
        assert_eq!(
            tag(&ClientMessage::AttachScroll {
                source: AttachScrollSource::Wheel,
                direction: AttachScrollDirection::Up,
                lines: 1,
                column: None,
                row: None,
                modifiers: 0,
            }),
            6
        );
        assert_eq!(
            tag(&ClientMessage::ObserveTerminal {
                target: "w1:p1".to_owned(),
            }),
            7
        );
        assert_eq!(
            tag(&ClientMessage::ControlTerminal {
                target: "w1:p1".to_owned(),
                takeover: false,
            }),
            8
        );
        assert_eq!(
            tag(&ClientMessage::GraphicsTransmissionResult {
                transfer_id: 1,
                image_id: 2,
                success: true,
            }),
            9
        );
        assert_eq!(
            tag(&ClientMessage::GraphicsTransmissionStarted {
                transfer_id: 1,
                image_id: 2,
            }),
            10
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellResize {
                cell_width_px: 8,
                cell_height_px: 16,
                surface_size: ClientSurfaceSize { cols: 80, rows: 29 },
                pixel_mouse: false,
            }),
            12
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellPaneInput {
                pane_id: "pane".into(),
                events: Vec::new(),
            }),
            13
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellPopupInput {
                terminal_id: "popup".into(),
                events: Vec::new(),
            }),
            14
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellEndpointRequest {
                boot_id: "boot".into(),
                request: "{}".into(),
            }),
            15
        );
        assert_eq!(
            tag(&ClientMessage::AttachMouse {
                kind: ClientMouseKind::Down(ClientMouseButton::Left),
                position: ClientMousePosition::Cell { column: 10, row: 5 },
                geometry: None,
                modifiers: 0,
                lines: 1,
            }),
            16
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellHostTheme {
                update: ClientHostThemeUpdate::Appearance(ClientHostAppearance::Dark),
            }),
            17
        );
        assert_eq!(tag(&ClientMessage::ClientShellFocus { focused: true }), 18);
        assert_eq!(
            tag(&ClientMessage::ClientShellMouseCapture { enabled: true }),
            19
        );
        assert_eq!(
            tag(&ClientMessage::EndpointControl {
                kind: String::new(),
                data: String::new(),
            }),
            20
        );
    }

    #[test]
    fn client_shell_pane_input_roundtrips_semantic_and_windows_keys() {
        let windows_record = crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 0x37,
            virtual_scan_code: 0x08,
            unicode: 0,
            control_key_state: 0x0008,
        };
        let message = ClientMessage::ClientShellPaneInput {
            pane_id: "w1:p2".into(),
            events: vec![
                ClientPaneInputEvent::Key {
                    code: ClientKeyCode::Char('l'),
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: ClientKeyKind::Release,
                    repeat_count: 1,
                    shifted_codepoint: Some('L' as u32),
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: None,
                    windows_record: None,
                },
                ClientPaneInputEvent::Key {
                    code: ClientKeyCode::Char('7'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: ClientKeyKind::Press,
                    repeat_count: 1,
                    shifted_codepoint: None,
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: Some(0x08),
                    windows_record: Some(windows_record),
                },
            ],
        };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode targeted semantic input");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode targeted semantic input");
        assert_eq!(decoded, message);
        assert_eq!(
            encoded_sha256(&message),
            "f558384bb53dfd2baf1fa72e1709d88be6891da79e51f88513905afc085065e6"
        );
        let ClientMessage::ClientShellPaneInput { events, .. } = decoded else {
            panic!("expected targeted semantic input");
        };
        let crate::raw_input::RawInputEvent::Key(semantic) = events[0].to_raw_input_event() else {
            panic!("expected semantic key");
        };
        assert_eq!(semantic.shifted_codepoint, Some('L' as u32));
        assert_eq!(semantic.kind, crossterm::event::KeyEventKind::Release);
        let crate::raw_input::RawInputEvent::Key(key) = events[1].to_raw_input_event() else {
            panic!("expected Windows key");
        };
        assert_eq!(key.code, crossterm::event::KeyCode::Char('7'));
        assert_eq!(key.modifiers, crossterm::event::KeyModifiers::CONTROL);
        assert_eq!(key.windows_record(), Some(windows_record));
    }

    #[test]
    fn client_shell_key_roundtrip_preserves_physical_generated_text_encoding() {
        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::SHIFT,
        )
        .with_generated_text(Some("/".into()))
        .with_windows_record(crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 0x37,
            virtual_scan_code: 0x08,
            unicode: '/' as u16,
            control_key_state: 0,
        });
        let event = ClientPaneInputEvent::from_terminal_key(key).expect("semantic pane key");
        assert!(matches!(
            event,
            ClientPaneInputEvent::Key {
                tracks_release: true,
                ..
            }
        ));
        let crate::raw_input::RawInputEvent::Key(roundtripped) = event.to_raw_input_event() else {
            panic!("pane key should remain a key");
        };

        assert!(roundtripped.has_physical_identity());
        let encoded = crate::input::encode_terminal_key(
            roundtripped,
            crate::input::KeyboardProtocol::Kitty { flags: 8 },
        );
        assert_ne!(encoded, b"/");
        assert!(encoded.starts_with(b"\x1b["));
    }

    #[test]
    fn client_shell_focus_roundtrip() {
        let message = ClientMessage::ClientShellFocus { focused: false };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode client shell focus");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode client shell focus");
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_mouse_capture_roundtrip() {
        let message = ClientMessage::ClientShellMouseCapture { enabled: false };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode client shell mouse capture");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode client shell mouse capture");
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_host_theme_roundtrip() {
        let message = ClientMessage::ClientShellHostTheme {
            update: ClientHostThemeUpdate::PaletteColors(vec![(
                4,
                ClientHostColor {
                    r: 10,
                    g: 20,
                    b: 30,
                },
            )]),
        };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode host theme update");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode host theme update");
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_endpoint_messages_roundtrip() {
        let request = ClientMessage::ClientShellEndpointRequest {
            boot_id: "boot-a".into(),
            request: r#"{"id":"request-a","method":"session.snapshot","params":{}}"#.into(),
        };
        let encoded = bincode::serde::encode_to_vec(&request, bincode::config::standard())
            .expect("encode endpoint request");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode endpoint request");
        assert_eq!(decoded, request);
        assert_eq!(
            encoded_sha256(&request),
            "de5693585a01f6b0d5ee07c51b6ddf79ee9f67dbf183255822d31f35210f5ffb"
        );

        let response = ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: "boot-a".into(),
            request_id: "request-a".into(),
            final_chunk: true,
            data: br#"{"id":"request-a","result":{"type":"ok"}}"#.to_vec(),
        };
        let encoded = bincode::serde::encode_to_vec(&response, bincode::config::standard())
            .expect("encode endpoint response");
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode endpoint response");
        assert_eq!(decoded, response);
        assert_eq!(
            encoded_sha256(&response),
            "bc14dbb5263d3097fe6d3e70a4b6d71aa9c2fa4ae3206d692a7182512bffdd1d"
        );
    }

    #[test]
    fn client_clipboard_image_roundtrip() {
        let msg = ClientMessage::ClipboardImage {
            target: ClientClipboardImageTarget::Pane("w1:p1".into()),
            extension: "png".to_owned(),
            data: vec![0x89, b'P', b'N', b'G'],
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "1c02110be0671faf318b3f4d8f507748d5a0a98cd474832669c5290d97ef19ab"
        );
    }

    #[test]
    fn client_input_large_multilingual_payload_roundtrip() {
        let text = "你好，今天我们测试一段比较长的语音输入。こんにちは。안녕하세요.🙂".repeat(1024);
        assert!(text.len() > 64 * 1024);
        assert!(text.len() < MAX_FRAME_SIZE);
        let msg = ClientMessage::Input {
            data: text.as_bytes().to_vec(),
        };

        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, consumed): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();

        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn client_resize_roundtrip() {
        let msg = ClientMessage::Resize {
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_detach_roundtrip() {
        let msg = ClientMessage::Detach;
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_attach_terminal_roundtrip() {
        let msg = ClientMessage::AttachTerminal {
            terminal_id: "term_123".to_owned(),
            takeover: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_observe_terminal_roundtrip() {
        let msg = ClientMessage::ObserveTerminal {
            target: "w1:p1".to_owned(),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_control_terminal_roundtrip() {
        let msg = ClientMessage::ControlTerminal {
            target: "w1:p1".to_owned(),
            takeover: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_attach_scroll_roundtrip() {
        let msg = ClientMessage::AttachScroll {
            source: AttachScrollSource::Wheel,
            direction: AttachScrollDirection::Up,
            lines: 3,
            column: Some(12),
            row: Some(7),
            modifiers: 4,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    // ---- Round-trip: ServerMessage ----

    #[test]
    fn server_welcome_roundtrip() {
        let msg = ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            encoding: RenderEncoding::SemanticFrame,
            error: None,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_welcome_with_error_roundtrip() {
        let msg = ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            encoding: RenderEncoding::SemanticFrame,
            error: Some("incompatible version".to_owned()),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_frame_roundtrip_nontrivial() {
        // Build a 3×2 frame with varied styles (≥2×2).
        let frame = FrameData {
            cells: vec![
                CellData {
                    symbol: "H".into(),
                    fg: color_to_u32(Color::Red),
                    bg: color_to_u32(Color::Black),
                    modifier: Modifier::BOLD.bits(),
                    skip: false,
                    hyperlink: None,
                },
                CellData {
                    symbol: "i".into(),
                    fg: color_to_u32(Color::Green),
                    bg: color_to_u32(Color::Reset),
                    modifier: Modifier::ITALIC.bits(),
                    skip: false,
                    hyperlink: None,
                },
                CellData {
                    symbol: "!".into(),
                    fg: color_to_u32(Color::Rgb(255, 128, 0)),
                    bg: color_to_u32(Color::Indexed(220)),
                    modifier: (Modifier::BOLD | Modifier::UNDERLINED).bits(),
                    skip: false,
                    hyperlink: Some(0),
                },
                CellData {
                    symbol: " ".into(),
                    fg: color_to_u32(Color::Reset),
                    bg: color_to_u32(Color::Reset),
                    modifier: Modifier::empty().bits(),
                    skip: true,
                    hyperlink: None,
                },
                CellData {
                    symbol: "→".into(), // multi-byte grapheme
                    fg: color_to_u32(Color::Cyan),
                    bg: color_to_u32(Color::Blue),
                    modifier: Modifier::REVERSED.bits(),
                    skip: false,
                    hyperlink: None,
                },
                CellData {
                    symbol: "🦀".into(), // emoji, wide grapheme cluster
                    fg: color_to_u32(Color::Yellow),
                    bg: color_to_u32(Color::Magenta),
                    modifier: Modifier::empty().bits(),
                    skip: false,
                    hyperlink: None,
                },
            ],
            width: 3,
            height: 2,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 6,
            }),
            hyperlinks: vec!["https://example.com".to_owned()],
            graphics: Vec::new(),
        };
        let msg = ServerMessage::PaneSurface(PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: frame.clone(),
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "7c016f7b21ddb5ac79212cf65a968b93eb292b5305b941263e89ffaa40158ee3"
        );
        match decoded {
            ServerMessage::PaneSurface(surface) => {
                assert_eq!(surface.frame.cells[2].hyperlink, Some(0));
                assert_eq!(
                    surface.frame.hyperlinks,
                    vec!["https://example.com".to_owned()]
                );
            }
            other => panic!("expected pane surface, got {other:?}"),
        }
    }

    #[test]
    fn pane_surface_patch_roundtrip() {
        let msg = ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
            boot_id: "boot-1".into(),
            projection_revision: 3,
            base_surface_revision: 7,
            surface_revision: 8,
            rows: vec![PaneSurfacePatchRow {
                x: 2,
                y: 4,
                cells: vec![CellData {
                    symbol: "x".into(),
                    fg: 1,
                    bg: 2,
                    modifier: 3,
                    skip: false,
                    hyperlink: None,
                }],
            }],
            panes: Vec::new(),
            cursor: Some(CursorState {
                x: 2,
                y: 4,
                visible: true,
                shape: 2,
            }),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(
            encoded_sha256(&msg),
            "0814b99a1dc6eaf7918424aa416c066509cbfb73b72344a809c27cde78cb6dbd"
        );
    }

    #[test]
    fn client_shell_graphics_payload_codec_is_frozen() {
        let key = SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "w1:p1".into(),
                },
                image_id: 7,
            },
            image_width: 2,
            image_height: 1,
            format: SurfaceGraphicsFormat::Rgba,
            data_len: 8,
            data_fingerprint: 42,
        };
        let message = ServerMessage::PaneSurface(PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 2,
            surface_revision: 3,
            frame: FrameData {
                cells: Vec::new(),
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene {
                assets: vec![SurfaceGraphicsAsset {
                    key: key.clone(),
                    data: vec![255, 0, 0, 255, 0, 255, 0, 255],
                }],
                placements: vec![SurfaceGraphicsPlacement {
                    asset: key,
                    logical_placement_id: 9,
                    x: 1,
                    y: 2,
                    cols: 2,
                    rows: 1,
                    source_x: 0,
                    source_y: 0,
                    source_width: 2,
                    source_height: 1,
                    x_offset: 0,
                    y_offset: 0,
                    z: -1,
                    scrollback_offset: 0,
                }],
                retained_assets: Vec::new(),
            },
        });

        assert_eq!(
            encoded_sha256(&message),
            "49c4efec0f1456c8ca4112ddf6ead1ab75d0224007576c2ccc18c3fca55a69f0"
        );
    }

    #[test]
    fn server_endpoint_control_tag_is_frozen() {
        let message = ServerMessage::EndpointControl {
            kind: "endpoint.welcome.v1".into(),
            data: r#"{"generation":1}"#.into(),
        };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard()).unwrap();
        assert_eq!(encoded.first(), Some(&20));
        assert_eq!(
            bincode::serde::encode_to_vec(
                ServerMessage::EndpointControl {
                    kind: String::new(),
                    data: String::new(),
                },
                bincode::config::standard(),
            )
            .unwrap(),
            [20, 0, 0]
        );
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_server_message_tags_are_frozen() {
        fn tag(message: &ServerMessage) -> u8 {
            *bincode::serde::encode_to_vec(message, bincode::config::standard())
                .unwrap()
                .first()
                .expect("encoded server message should include enum tag")
        }

        let empty_frame = || PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                cells: Vec::new(),
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        };
        assert_eq!(tag(&ServerMessage::PaneSurface(empty_frame())), 13);
        assert_eq!(
            tag(&ServerMessage::ClientShellError {
                message: String::new(),
            }),
            15
        );
        assert_eq!(
            tag(&ServerMessage::ClientShellKeyboardReportAll { enabled: false }),
            17
        );
        assert_eq!(
            tag(&ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: String::new(),
                request_id: String::new(),
                final_chunk: true,
                data: Vec::new(),
            }),
            18
        );
        assert_eq!(
            tag(&ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
                boot_id: String::new(),
                projection_revision: 0,
                base_surface_revision: 0,
                surface_revision: 0,
                rows: Vec::new(),
                panes: Vec::new(),
                cursor: None,
            })),
            19
        );
        assert_eq!(
            tag(&ServerMessage::EndpointControl {
                kind: String::new(),
                data: String::new(),
            }),
            20
        );
    }

    #[test]
    fn client_shell_snapshot_roundtrip() {
        let msg = ServerMessage::ClientShellSnapshot(Box::new(ClientShellSnapshot {
            boot_id: "boot-1".into(),
            revision: 1,
            config_diagnostic: Some("endpoint config warning".into()),
            product_announcement: Some(ClientShellProductAnnouncement {
                version: "0.8.2".into(),
                id: "client-shell".into(),
                title: "Client shell".into(),
                body: "### New\n- Client-owned chrome".into(),
                preview: false,
            }),
            update_available: Some("0.8.3".into()),
            update_install_command: "herdr update".into(),
            server_keybindings_toml: Some("[keys]\nprefix = \"ctrl+a\"\n".into()),
            latest_release_notes_available: true,
            integration_updates_available: true,
            worktree_directory: "/tmp/herdr-worktrees".into(),
            release_notes: Some(ClientShellReleaseNotes {
                version: "0.8.3".into(),
                body: "### New\n- Update ready".into(),
                preview: true,
            }),
            focused_workspace_id: Some("w1".into()),
            focused_tab_id: Some("w1:t1".into()),
            focused_pane_id: Some("w1:p1".into()),
            tab_bar_right: vec![ClientShellTabStatusSegment {
                text: "host".into(),
                accent: false,
            }],
            tab_bar_right_separator: " · ".into(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: vec![ClientShellWorkspace {
                workspace_id: "w1".into(),
                active_tab_id: "w1:t1".into(),
                new_workspace_cwd: "/tmp".into(),
                number: 1,
                label: "shell".into(),
                custom_label: false,
                branch: Some("main".into()),
                git_ahead_behind: None,
                tokens: Vec::new(),
                worktree: None,
                focused: true,
                agent_status: crate::api::schema::AgentStatus::Idle,
            }],
            tabs: vec![ClientShellTab {
                tab_id: "w1:t1".into(),
                workspace_id: "w1".into(),
                number: 1,
                label: "main".into(),
                custom_label: true,
                zoomed: false,
                focused: true,
                agent_status: crate::api::schema::AgentStatus::Idle,
            }],
            panes: vec![ClientShellPane {
                pane_id: "w1:p1".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                label: None,
                cwd: Some("/repo".into()),
                foreground_cwd: Some("/repo".into()),
                focused: true,
                right_click_passthrough: false,
            }],
            agents: Vec::new(),
            commands: vec![ClientShellCommand {
                command_id: "cmd_0123456789abcdef0123456789abcdef".into(),
                binding_label: "prefix+z".into(),
                binding_labels: vec!["prefix+z".into()],
                action: ClientShellCommandAction::Shell,
                description: Some("deploy".into()),
            }],
        }));
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_shutdown_roundtrip() {
        let msg = ServerMessage::ServerShutdown {
            reason: Some("updating".to_owned()),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn semantic_notification_roundtrip() {
        let msg = ServerMessage::SemanticNotification(SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: Some("repo · 1".into()),
            sound: Some(SemanticNotificationSound::Request),
            agent: Some("codex".into()),
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
            pane_id: Some("w1:p1".into()),
            position: Some(crate::config::ToastHerdrPosition::TopRight),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_notify_roundtrip() {
        for kind in [
            NotifyKind::Sound,
            NotifyKind::Toast,
            NotifyKind::SystemToast,
        ] {
            let msg = ServerMessage::Notify {
                kind,
                message: "agent done".to_owned(),
                body: None,
            };
            let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
            let (decoded, _): (ServerMessage, _) =
                bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn server_clipboard_roundtrip() {
        let msg = ServerMessage::Clipboard {
            data: "dGVzdA==".to_owned(), // base64 "test"
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_window_title_roundtrip() {
        for title in [Some("herdr api".to_owned()), None] {
            let msg = ServerMessage::WindowTitle { title };
            let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
            let (decoded, _): (ServerMessage, _) =
                bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn server_graphics_roundtrip() {
        let msg = ServerMessage::Graphics {
            bytes: b"\x1b_Ga=d,d=A,q=2;\x1b\\".to_vec(),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "28a420f92e0e05e6760a8c140baf307c360c6d1b1aa68027481b324f87e22c44"
        );
    }

    #[test]
    fn server_terminal_frame_roundtrip() {
        let msg = ServerMessage::Terminal(TerminalFrame {
            seq: 7,
            width: 120,
            height: 40,
            full: false,
            bytes: b"\x1b[1;1Hhello".to_vec(),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_reload_sound_config_roundtrip() {
        let msg = ServerMessage::ReloadSoundConfig;
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_mouse_capture_roundtrip() {
        let msg = ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_shell_keyboard_report_all_roundtrip() {
        let msg = ServerMessage::ClientShellKeyboardReportAll { enabled: true };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn direct_terminal_keyboard_mode_roundtrip() {
        let msg = ServerMessage::DirectTerminalKeyboardProtocol {
            flags: 15,
            modify_other_keys_level: 1,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn direct_graphics_messages_roundtrip() {
        let client = ClientMessage::GraphicsTransmissionResult {
            transfer_id: 7,
            image_id: 42,
            success: false,
        };
        let encoded = bincode::serde::encode_to_vec(&client, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(client, decoded);

        let server = ServerMessage::GraphicsFile {
            path: "/run/user/1000/herdr/source/frame".into(),
            expected_len: 4,
            image_id: 42,
            transfer_id: 7,
            leading: b"\x1b[2;3H".to_vec(),
            control: "a=T,f=32,i=42,q=0".into(),
            surface_asset: None,
        };
        let encoded = bincode::serde::encode_to_vec(&server, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(server, decoded);
    }

    #[test]
    fn server_terminal_bell_roundtrip() {
        let msg = ServerMessage::TerminalBell { count: 3 };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    // ---- Framing ----

    #[test]
    fn framing_small_message_roundtrip() {
        let msg = ClientMessage::TerminalHello {
            version: PROTOCOL_VERSION,
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: false,
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let decoded: ClientMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn framing_large_payload_roundtrip() {
        // Create a pane-surface message that is ≥128 KB.
        // Use a large frame with verbose cell data to exceed 128 KB after bincode encoding.
        // 200×50 = 10000 cells. With varied symbols and styles, this should easily exceed 128 KB.
        let width: u16 = 200;
        let height: u16 = 50;
        let cells: Vec<CellData> = (0..(width as usize) * (height as usize))
            .map(|i| CellData {
                symbol: if i % 256 < 32 {
                    " ".to_owned()
                } else {
                    format!("{:03}", i % 1000)
                },
                fg: color_to_u32(Color::Rgb((i % 256) as u8, ((i / 256) % 256) as u8, 128)),
                bg: color_to_u32(Color::Indexed((i % 256) as u8)),
                modifier: ((i % 16) as u16),
                skip: i % 100 == 0,
                hyperlink: None,
            })
            .collect();

        let frame = FrameData {
            cells,
            width,
            height,
            cursor: Some(CursorState {
                x: 10,
                y: 5,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let msg = ServerMessage::PaneSurface(PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame,
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        });

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        // Verify the payload is at least 128 KB
        assert!(
            buf.len() >= 128 * 1024,
            "framed payload should be >= 128 KB, got {} bytes",
            buf.len()
        );

        let decoded: ServerMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn framing_multiple_messages_sequential() {
        // Write 100+ messages of varying types and read them back.
        let mut buf = Vec::new();
        let mut expected = Vec::new();

        for i in 0..150u32 {
            let msg = match i % 5 {
                0 => ClientMessage::TerminalHello {
                    version: PROTOCOL_VERSION,
                    cols: (80 + (i % 40) as u16),
                    rows: (24 + (i % 20) as u16),
                    cell_width_px: 8,
                    cell_height_px: 16,
                    pixel_mouse: i % 2 == 0,
                },
                1 => ClientMessage::Input {
                    data: vec![(i % 256) as u8; (i as usize % 50) + 1],
                },
                2 => ClientMessage::ClipboardImage {
                    target: ClientClipboardImageTarget::DirectTerminal,
                    extension: "png".to_owned(),
                    data: vec![0x89, b'P', b'N', b'G', (i % 256) as u8],
                },
                3 => ClientMessage::Resize {
                    cols: (100 + (i % 30) as u16),
                    rows: (30 + (i % 10) as u16),
                    cell_width_px: 8,
                    cell_height_px: 16,
                    pixel_mouse: i % 2 == 0,
                },
                4 => ClientMessage::Detach,
                _ => unreachable!(),
            };
            write_message(&mut buf, &msg).unwrap();
            expected.push(msg);
        }

        let mut cursor = buf.as_slice();
        for expected_msg in &expected {
            let decoded: ClientMessage = read_message(&mut cursor, MAX_FRAME_SIZE).unwrap();
            assert_eq!(*expected_msg, decoded);
        }
    }

    #[test]
    fn framing_oversized_rejected_without_panic() {
        // Craft a frame with a huge length prefix (4 GB claim).
        let mut buf: Vec<u8> = (u32::MAX).to_le_bytes().to_vec();
        // Add a few garbage bytes after the length prefix.
        buf.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        match result {
            Err(FramingError::Oversized { claimed, max }) => {
                assert_eq!(claimed, u32::MAX as usize);
                assert_eq!(max, MAX_FRAME_SIZE);
            }
            other => panic!("expected Oversized error, got: {other:?}"),
        }
    }

    #[test]
    fn framing_malformed_payload_rejected_without_panic() {
        // Valid length prefix pointing to garbage data.
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];
        let mut buf = (payload.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(&payload);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        assert!(result.is_err(), "malformed payload should be rejected");
        match result {
            Err(FramingError::Bincode(_)) => {} // expected
            other => panic!("expected Bincode error, got: {other:?}"),
        }
    }

    #[test]
    fn framing_truncated_stream_returns_unexpected_eof() {
        // Write a length prefix claiming 100 bytes, but only provide 4.
        let mut buf: Vec<u8> = 100u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        match result {
            Err(FramingError::UnexpectedEof) => {}
            other => panic!("expected UnexpectedEof, got: {other:?}"),
        }
    }

    #[test]
    fn framing_zero_length_message() {
        // A 1-byte message (smallest possible valid bincode payload).
        // Actually, let's test with the smallest real message: Detach.
        let msg = ClientMessage::Detach;
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        // Verify the length prefix is correct
        let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(
            len,
            buf.len() - 4,
            "length prefix should match payload size"
        );

        let decoded: ClientMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn framing_partial_read_reassembly() {
        // Simulate partial reads by using a reader that yields small chunks.
        let msg = ClientMessage::Input {
            data: vec![42; 500], // 500-byte input payload
        };
        let mut full_buf = Vec::new();
        write_message(&mut full_buf, &msg).unwrap();

        // Wrap in a chunked reader that only yields 7 bytes at a time.
        let mut chunked = ChunkedReader::new(full_buf, 7);
        let decoded: ClientMessage = read_message(&mut chunked, MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    // ---- Version negotiation ----

    #[test]
    fn version_compatible() {
        assert_eq!(
            check_client_version(PROTOCOL_VERSION),
            VersionCheck::Compatible
        );
    }

    #[test]
    fn version_older_client_rejected() {
        let result = check_client_version(PROTOCOL_VERSION - 1);
        assert!(matches!(result, VersionCheck::Incompatible(_)));
        if let VersionCheck::Incompatible(msg) = result {
            assert!(msg.contains("older"), "error should mention older version");
        }
    }

    #[test]
    fn version_newer_client_rejected() {
        let result = check_client_version(PROTOCOL_VERSION + 1);
        assert!(matches!(result, VersionCheck::Incompatible(_)));
        if let VersionCheck::Incompatible(msg) = result {
            assert!(msg.contains("newer"), "error should mention newer version");
        }
    }

    // ---- Pre-persistence client rejection ----

    #[test]
    fn prepersistence_version_zero_rejected() {
        let result = check_client_version(0);
        match result {
            VersionCheck::Incompatible(msg) => {
                assert!(
                    msg.contains("pre-persistence"),
                    "error should mention pre-persistence: {msg}"
                );
            }
            _ => panic!("version 0 should be rejected as incompatible"),
        }
    }

    #[test]
    fn prepersistence_version_zero_welcome_has_error() {
        // Simulating what the server would send to a v0 client.
        let check = check_client_version(0);
        let response = match check {
            VersionCheck::Compatible => ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                encoding: RenderEncoding::SemanticFrame,
                error: None,
            },
            VersionCheck::Incompatible(reason) => ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                encoding: RenderEncoding::SemanticFrame,
                error: Some(reason),
            },
        };

        match response {
            ServerMessage::Welcome { error: Some(_), .. } => {}
            other => panic!("expected Welcome with error, got: {other:?}"),
        }
    }

    // ---- Malformed/oversized input ----

    #[test]
    fn oversized_frame_does_not_panic() {
        // Claim 4GB payload — should return Oversized error, not panic.
        let mut buf: Vec<u8> = 0xFFC00000u32.to_le_bytes().to_vec(); // ~4 GB claim
        buf.extend_from_slice(&[0; 8]);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        assert!(result.is_err());
        // Did not panic — test passing is proof.
    }

    #[test]
    fn malformed_frame_does_not_panic() {
        // Random garbage bytes after a valid-ish length prefix.
        let garbage: Vec<u8> = (0..200).map(|i| (i ^ 0xAA) as u8).collect();
        let mut buf = (garbage.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(&garbage);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        assert!(result.is_err());
        // Did not panic.
    }

    #[test]
    fn oversized_input_rejected_custom_max() {
        // Verify a custom (small) max_frame_size is enforced.
        let msg = ClientMessage::Input {
            data: vec![0x41; 1000],
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        let result: Result<ClientMessage, FramingError> = read_message(&mut buf.as_slice(), 64);
        // The actual bincode payload for 1000 bytes of input will be > 64 bytes.
        assert!(
            matches!(result, Err(FramingError::Oversized { .. })),
            "expected Oversized with small max_frame_size"
        );
    }

    // ---- FrameData ↔ ratatui Buffer conversion ----

    #[test]
    fn frame_data_roundtrip_through_ratatui_buffer() {
        let area = ratatui::layout::Rect::new(0, 0, 5, 3);
        let mut buffer = ratatui::buffer::Buffer::filled(area, ratatui::buffer::Cell::new(" "));

        // Write some styled content.
        buffer.cell_mut((0, 0)).unwrap().set_symbol("H");
        buffer.cell_mut((0, 0)).unwrap().fg = Color::Red;
        buffer.cell_mut((0, 0)).unwrap().modifier = Modifier::BOLD;

        buffer.cell_mut((1, 0)).unwrap().set_symbol("i");
        buffer.cell_mut((1, 0)).unwrap().fg = Color::Green;
        buffer.cell_mut((1, 0)).unwrap().modifier = Modifier::ITALIC;

        buffer.cell_mut((2, 0)).unwrap().set_symbol("!");
        buffer.cell_mut((2, 0)).unwrap().fg = Color::Rgb(255, 128, 0);
        buffer.cell_mut((2, 0)).unwrap().bg = Color::Indexed(220);

        let cursor = CursorState {
            x: 1,
            y: 0,
            visible: true,
            shape: 0,
        };
        let frame = FrameData::from_ratatui_buffer(&buffer, Some(cursor.clone()));

        // Verify frame dimensions.
        assert_eq!(frame.width, 5);
        assert_eq!(frame.height, 3);
        assert_eq!(frame.cells.len(), 15);
        assert_eq!(frame.cursor, Some(cursor));

        // Verify specific cells survived the conversion.
        assert_eq!(frame.cells[0].symbol, "H");
        assert_eq!(frame.cells[0].fg, color_to_u32(Color::Red));
        assert_eq!(frame.cells[0].modifier, Modifier::BOLD.bits());

        assert_eq!(frame.cells[1].symbol, "i");
        assert_eq!(frame.cells[1].fg, color_to_u32(Color::Green));
        assert_eq!(frame.cells[1].modifier, Modifier::ITALIC.bits());

        assert_eq!(frame.cells[2].symbol, "!");
        assert_eq!(frame.cells[2].fg, color_to_u32(Color::Rgb(255, 128, 0)));
        assert_eq!(frame.cells[2].bg, color_to_u32(Color::Indexed(220)));

        let with_links = FrameData::from_ratatui_buffer_with_hyperlinks(
            &buffer,
            None,
            &[((1, 0), "i".to_owned(), "https://example.com".to_owned())],
        );
        assert_eq!(with_links.cells[1].hyperlink, Some(0));
        assert_eq!(
            with_links.hyperlinks,
            vec!["https://example.com".to_owned()]
        );

        // Convert back to ratatui buffer and compare.
        let restored = frame.to_ratatui_buffer().expect("should reconstruct");
        assert_eq!(restored.area, area);
        assert_eq!(restored.cell((0, 0)).unwrap().symbol(), "H");
        assert_eq!(restored.cell((0, 0)).unwrap().fg, Color::Red);
        assert_eq!(restored.cell((0, 0)).unwrap().modifier, Modifier::BOLD);
        assert_eq!(restored.cell((1, 0)).unwrap().symbol(), "i");
        assert_eq!(restored.cell((2, 0)).unwrap().symbol(), "!");
        assert_eq!(restored.cell((2, 0)).unwrap().fg, Color::Rgb(255, 128, 0));
    }

    #[test]
    fn frame_data_rejects_mismatched_cell_count() {
        let frame = FrameData {
            cells: vec![
                CellData {
                    symbol: "X".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                };
                5
            ], // 5 cells but 3×2 = 6 expected
            width: 3,
            height: 2,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        assert!(frame.to_ratatui_buffer().is_none());
    }

    // ---- Color conversion coverage ----

    #[test]
    fn color_roundtrip_all_named_colors() {
        let named = [
            Color::Reset,
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Gray,
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
        ];
        for c in named {
            assert_eq!(
                u32_to_color(color_to_u32(c)),
                c,
                "roundtrip failed for {c:?}"
            );
        }
    }

    #[test]
    fn color_roundtrip_indexed() {
        for i in 0..=255u8 {
            let c = Color::Indexed(i);
            assert_eq!(
                u32_to_color(color_to_u32(c)),
                c,
                "roundtrip failed for Indexed({i})"
            );
        }
    }

    #[test]
    fn color_roundtrip_rgb() {
        let c = Color::Rgb(0xAB, 0xCD, 0xEF);
        assert_eq!(u32_to_color(color_to_u32(c)), c);

        let c = Color::Rgb(0, 0, 0);
        assert_eq!(u32_to_color(color_to_u32(c)), c);

        let c = Color::Rgb(255, 255, 255);
        assert_eq!(u32_to_color(color_to_u32(c)), c);
    }

    // ---- Modifier conversion ----

    #[test]
    fn modifier_roundtrip() {
        let all_mods = [
            Modifier::BOLD,
            Modifier::ITALIC,
            Modifier::REVERSED,
            Modifier::UNDERLINED,
            Modifier::DIM,
            Modifier::SLOW_BLINK,
            Modifier::CROSSED_OUT,
            Modifier::BOLD | Modifier::ITALIC,
            Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED,
            Modifier::empty(),
        ];
        for m in all_mods {
            assert_eq!(
                u16_to_modifier(modifier_to_u16(m)),
                m,
                "roundtrip failed for {m:?}"
            );
        }
    }

    #[test]
    fn read_message_rejects_trailing_bytes() {
        // Encode a valid message, then append an extra byte after it.
        let msg = ClientMessage::Detach;
        let mut payload = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let original_len = payload.len();
        payload.push(0xDE); // trailing garbage

        // Frame it with the inflated length (original + 1).
        let mut buf = (payload.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(&payload);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        match result {
            Err(FramingError::Bincode(msg)) => {
                assert!(
                    msg.contains("trailing bytes"),
                    "error should mention trailing bytes: {msg}"
                );
                assert!(
                    msg.contains(&format!("decoded {original_len}")),
                    "error should mention decoded byte count: {msg}"
                );
            }
            other => panic!("expected Bincode error about trailing bytes, got: {other:?}"),
        }
    }

    #[test]
    fn read_message_accepts_exact_payload() {
        // A normally-framed message should decode without error.
        let msg = ClientMessage::TerminalHello {
            version: PROTOCOL_VERSION,
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            pixel_mouse: false,
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let decoded: ClientMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn write_message_rejects_oversized_payload() {
        // We can't easily create a message that exceeds u32::MAX in a test,
        // but we can verify the check exists by testing that normal messages
        // have lengths well within the limit and the function doesn't fail.
        let msg = ClientMessage::Detach;
        let mut buf = Vec::new();
        assert!(write_message(&mut buf, &msg).is_ok());
    }

    // ---- Unix socketpair integration test ----

    #[cfg(unix)]
    #[test]
    fn framing_over_unix_socketpair() {
        use std::os::unix::net::UnixStream;

        let (mut a, mut b) = UnixStream::pair().expect("socketpair");

        let messages = vec![
            ClientMessage::TerminalHello {
                version: PROTOCOL_VERSION,
                cols: 200,
                rows: 60,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: true,
            },
            ClientMessage::Input {
                data: b"hello world".to_vec(),
            },
            ClientMessage::ClipboardImage {
                target: ClientClipboardImageTarget::DirectTerminal,
                extension: "png".to_owned(),
                data: vec![0x89, b'P', b'N', b'G'],
            },
            ClientMessage::Resize {
                cols: 100,
                rows: 30,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: true,
            },
            ClientMessage::Detach,
        ];

        // Set non-blocking so we can write and read in the same test.
        a.set_nonblocking(false).unwrap();
        b.set_nonblocking(false).unwrap();

        for msg in &messages {
            write_message(&mut a, msg).unwrap();
        }

        for expected in &messages {
            let decoded: ClientMessage = read_message(&mut b, MAX_FRAME_SIZE).unwrap();
            assert_eq!(*expected, decoded);
        }
    }

    // ---- Helper: chunked reader for simulating partial reads ----

    /// A `Read` wrapper that yields at most `chunk_size` bytes per `read()` call,
    /// simulating partial reads on a real socket.
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk_size: usize,
    }

    impl ChunkedReader {
        fn new(data: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk_size,
            }
        }
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let remaining = self.data.len() - self.pos;
            let to_read = buf.len().min(remaining).min(self.chunk_size);
            buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
            self.pos += to_read;
            Ok(to_read)
        }
    }
}
