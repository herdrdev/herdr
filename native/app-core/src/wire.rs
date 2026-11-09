//! The part of the server's client protocol this app speaks.
//!
//! Frames are `[u32 LE length][bincode payload]` (bincode 2, standard config). The enums below keep the
//! variant *positions* of the server's `ClientMessage` / `ServerMessage` (`src/protocol/wire.rs`); variants
//! this app never sends are placeholders so the indices of the others stay right. The server marks those
//! enums append-only for the stable endpoint generation, so the positions do not move.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

const MAX_FRAME: usize = 32 * 1024 * 1024;

fn config() -> impl bincode::config::Config {
    bincode::config::standard()
}

pub fn write_frame<W: Write>(writer: &mut W, message: &impl Serialize) -> io::Result<()> {
    let payload = bincode::serde::encode_to_vec(message, config())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes"),
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// Modifier bits as the server reads them (crossterm's).
pub mod modifiers {
    pub const SHIFT: u8 = 0b0001;
    pub const CONTROL: u8 = 0b0010;
    pub const ALT: u8 = 0b0100;
}

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
        /// Windows only; always `None` here.
        windows_record: Option<u8>,
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

/// Client → server. Variants 0..=11, 14..=19 are never sent by this app (placeholders keep the indices).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(dead_code)] // placeholders are never constructed
pub enum ClientMessage {
    V0,
    V1,
    V2,
    V3,
    V4,
    V5,
    V6,
    V7,
    V8,
    V9,
    V10,
    V11,
    ClientShellResize {
        cell_width_px: u32,
        cell_height_px: u32,
        surface_size: ClientSurfaceSize,
        pixel_mouse: bool,
    },
    ClientShellPaneInput {
        pane_id: String,
        events: Vec<ClientPaneInputEvent>,
    },
    V14,
    V15,
    V16,
    V17,
    V18,
    V19,
    EndpointControl {
        kind: String,
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellData {
    pub symbol: String,
    pub fg: u32,
    pub bg: u32,
    pub modifier: u16,
    pub skip: bool,
    pub hyperlink: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    #[serde(default)]
    pub shape: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameData {
    pub cells: Vec<CellData>,
    pub width: u16,
    pub height: u16,
    pub cursor: Option<CursorState>,
    pub hyperlinks: Vec<String>,
    pub graphics: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceScrollMetrics {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u64,
}

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
pub enum PaneSurfaceSplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceSplit {
    pub direction: PaneSurfaceSplitDirection,
    pub pos: u16,
    pub area: SurfaceRect,
    pub hit_rect: SurfaceRect,
    pub path: Vec<bool>,
}

/// The leading fields of the server's `PaneSurfaceFrame`. The trailing `popup` and `graphics` are left
/// unread on purpose: bincode is not self-describing, but a prefix decodes fine and frames are
/// length-prefixed, so the rest is simply ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurface {
    pub boot_id: String,
    pub projection_revision: u64,
    pub surface_revision: u64,
    pub frame: FrameData,
    pub panes: Vec<PaneSurfacePane>,
    pub splits: Vec<PaneSurfaceSplit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePatchRow {
    pub x: u16,
    pub y: u16,
    pub cells: Vec<CellData>,
}

/// Cells changed since `base_surface_revision`, plus the current pane metadata and cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePatch {
    pub boot_id: String,
    pub projection_revision: u64,
    pub base_surface_revision: u64,
    pub surface_revision: u64,
    pub rows: Vec<PaneSurfacePatchRow>,
    pub panes: Vec<PaneSurfacePane>,
    pub cursor: Option<CursorState>,
}

/// The patch does not continue from the surface held (a lost or reordered update): ask for a full one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchMismatch {
    pub held: u64,
    pub base: u64,
}

impl PaneSurface {
    pub fn apply(&mut self, patch: PaneSurfacePatch) -> Result<(), PatchMismatch> {
        if patch.boot_id != self.boot_id
            || patch.projection_revision != self.projection_revision
            || patch.base_surface_revision != self.surface_revision
        {
            return Err(PatchMismatch {
                held: self.surface_revision,
                base: patch.base_surface_revision,
            });
        }
        let width = self.frame.width as usize;
        let height = self.frame.height as usize;
        for row in patch.rows {
            let (x, y) = (row.x as usize, row.y as usize);
            if y >= height || x >= width {
                continue;
            }
            for (i, cell) in row.cells.into_iter().enumerate().take(width - x) {
                self.frame.cells[y * width + x + i] = cell;
            }
        }
        self.frame.cursor = patch.cursor;
        self.panes = patch.panes;
        self.surface_revision = patch.surface_revision;
        Ok(())
    }
}

/// What the server sent, as far as this app cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Welcome {
        version: u32,
        error: Option<String>,
    },
    /// A named endpoint control (`endpoint.welcome.v1`, `shell.snapshot.v1`, …) with a JSON body.
    Control {
        kind: String,
        data: String,
    },
    Surface(Box<PaneSurface>),
    Patch(Box<PaneSurfacePatch>),
    /// Any other message, by variant index; ignored.
    Other(u32),
}

const SERVER_WELCOME: u32 = 0;
const SERVER_PANE_SURFACE: u32 = 13;
const SERVER_PANE_SURFACE_PATCH: u32 = 19;
const SERVER_ENDPOINT_CONTROL: u32 = 20;

pub fn parse_server_message(payload: &[u8]) -> Result<Incoming, String> {
    let (tag, used): (u32, usize) =
        bincode::serde::decode_from_slice(payload, config()).map_err(|e| e.to_string())?;
    let rest = &payload[used..];
    Ok(match tag {
        SERVER_WELCOME => {
            let ((version, _encoding, error), _): ((u32, u32, Option<String>), usize) =
                bincode::serde::decode_from_slice(rest, config()).map_err(|e| e.to_string())?;
            Incoming::Welcome { version, error }
        }
        SERVER_ENDPOINT_CONTROL => {
            let ((kind, data), _): ((String, String), usize) =
                bincode::serde::decode_from_slice(rest, config()).map_err(|e| e.to_string())?;
            Incoming::Control { kind, data }
        }
        SERVER_PANE_SURFACE => {
            let (surface, _): (PaneSurface, usize) =
                bincode::serde::decode_from_slice(rest, config()).map_err(|e| e.to_string())?;
            Incoming::Surface(Box::new(surface))
        }
        SERVER_PANE_SURFACE_PATCH => {
            let (patch, _): (PaneSurfacePatch, usize) =
                bincode::serde::decode_from_slice(rest, config()).map_err(|e| e.to_string())?;
            Incoming::Patch(Box::new(patch))
        }
        other => Incoming::Other(other),
    })
}

/// Colors as the server packs them: `0x00_00_00_NN` named (0 reset, 1..=16 the 16 ANSI colors),
/// `0x01_00_00_NN` palette index, `0x02_RR_GG_BB` truecolor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Reset,
    Named(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    pub fn unpack(value: u32) -> Self {
        match value >> 24 {
            0x01 => Color::Indexed((value & 0xFF) as u8),
            0x02 => Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8),
            _ => match (value & 0xFF) as u8 {
                0 => Color::Reset,
                n => Color::Named(n),
            },
        }
    }
}

/// Text attribute bits of `CellData::modifier` (ratatui's `Modifier`, low 12 bits).
pub mod style {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINED: u16 = 1 << 3;
    pub const REVERSED: u16 = 1 << 6;
    pub const CROSSED_OUT: u16 = 1 << 8;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode the server-side shapes with local mirrors and make sure our parser reads them back.
    #[derive(Serialize)]
    #[allow(dead_code)] // only some variants are built; the rest keep the indices of the server's enum
    enum ServerMirror {
        Welcome {
            version: u32,
            encoding: u32,
            error: Option<String>,
        },
        Terminal,
        Graphics,
        ServerShutdown,
        Notify,
        Clipboard,
        WindowTitle,
        ReloadSoundConfig,
        MouseCapture,
        TerminalBell,
        GraphicsFile,
        GraphicsTransmissionRetired,
        ClientShellSnapshot,
        PaneSurface(PaneSurface),
        SemanticNotification,
        ClientShellError,
        DirectTerminalKeyboardProtocol,
        ClientShellKeyboardReportAll,
        ClientShellEndpointResponseChunk,
        PaneSurfacePatch,
        EndpointControl {
            kind: String,
            data: String,
        },
    }

    fn payload(message: &impl Serialize) -> Vec<u8> {
        bincode::serde::encode_to_vec(message, config()).unwrap()
    }

    #[test]
    fn endpoint_control_is_variant_20_in_both_directions() {
        let sent = payload(&ClientMessage::EndpointControl {
            kind: "k".into(),
            data: "d".into(),
        });
        let mirrored = payload(&ServerMirror::EndpointControl {
            kind: "k".into(),
            data: "d".into(),
        });
        assert_eq!(sent, mirrored);
        assert_eq!(sent[0], 20);
        assert_eq!(
            parse_server_message(&mirrored).unwrap(),
            Incoming::Control {
                kind: "k".into(),
                data: "d".into()
            }
        );
    }

    #[test]
    fn pane_input_is_variant_13() {
        let sent = payload(&ClientMessage::ClientShellPaneInput {
            pane_id: "w1:p1".into(),
            events: vec![],
        });
        assert_eq!(sent[0], 13);
    }

    #[test]
    fn a_surface_with_trailing_fields_still_parses() {
        let surface = PaneSurface {
            boot_id: "boot".into(),
            projection_revision: 3,
            surface_revision: 9,
            frame: FrameData {
                cells: vec![CellData {
                    symbol: "x".into(),
                    fg: 0x02_10_20_30,
                    bg: 0,
                    modifier: 1,
                    skip: false,
                    hyperlink: None,
                }],
                width: 1,
                height: 1,
                cursor: Some(CursorState {
                    x: 0,
                    y: 0,
                    visible: true,
                    shape: 0,
                }),
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![],
            splits: vec![],
        };
        let mut bytes = payload(&ServerMirror::PaneSurface(surface.clone()));
        bytes.extend_from_slice(&[0, 0, 0, 0, 0]); // popup: None, graphics: ... (ignored)
        match parse_server_message(&bytes).unwrap() {
            Incoming::Surface(parsed) => assert_eq!(*parsed, surface),
            other => panic!("{other:?}"),
        }
    }

    fn cell(symbol: &str) -> CellData {
        CellData {
            symbol: symbol.into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn surface(width: u16, height: u16) -> PaneSurface {
        PaneSurface {
            boot_id: "b".into(),
            projection_revision: 1,
            surface_revision: 5,
            frame: FrameData {
                cells: vec![cell(" "); (width * height) as usize],
                width,
                height,
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![],
            splits: vec![],
        }
    }

    fn patch(base: u64, next: u64, x: u16, y: u16, text: &[&str]) -> PaneSurfacePatch {
        PaneSurfacePatch {
            boot_id: "b".into(),
            projection_revision: 1,
            base_surface_revision: base,
            surface_revision: next,
            rows: vec![PaneSurfacePatchRow {
                x,
                y,
                cells: text.iter().map(|s| cell(s)).collect(),
            }],
            panes: vec![],
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 0,
            }),
        }
    }

    #[test]
    fn patches_apply_in_order_and_a_gap_is_detected() {
        let mut s = surface(4, 2);
        s.apply(patch(5, 6, 1, 1, &["a", "b"])).unwrap();
        assert_eq!(s.frame.cells[4 + 1].symbol, "a");
        assert_eq!(s.frame.cells[4 + 2].symbol, "b");
        assert_eq!(s.surface_revision, 6);
        assert_eq!(s.frame.cursor.as_ref().map(|c| c.x), Some(2));
        // a patch built on revision 9 does not continue from 6
        assert_eq!(
            s.apply(patch(9, 10, 0, 0, &["z"])),
            Err(PatchMismatch { held: 6, base: 9 })
        );
        assert_eq!(
            s.frame.cells[0].symbol, " ",
            "a rejected patch changes nothing"
        );
        // spans running past the edge are clipped, not a panic
        s.apply(patch(6, 7, 3, 0, &["x", "y", "z"])).unwrap();
        assert_eq!(s.frame.cells[3].symbol, "x");
    }

    #[test]
    fn colors_unpack() {
        assert_eq!(Color::unpack(0), Color::Reset);
        assert_eq!(Color::unpack(0x0000_0003), Color::Named(3));
        assert_eq!(Color::unpack(0x0100_0007), Color::Indexed(7));
        assert_eq!(Color::unpack(0x02_0A_0B_0C), Color::Rgb(10, 11, 12));
    }
}
