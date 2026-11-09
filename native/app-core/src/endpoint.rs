//! Client of the server's stable endpoint (generation 1): the same door the TUI uses.
//!
//! After a JSON hello the server pushes the shell snapshot (workspaces, tabs, agents) and a rendered
//! surface of the active tab (a grid of cells for all its panes); the client sends semantic input.

use crate::wire::{
    parse_server_message, read_frame, write_frame, ClientKeyCode, ClientKeyKind, ClientMessage,
    ClientMouseKind, ClientMousePosition, ClientPaneInputEvent, ClientSurfaceSize, Incoming,
};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;

pub const HELLO: &str = "endpoint.hello.v1";
pub const WELCOME: &str = "endpoint.welcome.v1";
pub const SNAPSHOT: &str = "shell.snapshot.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
}

/// Reading side of a connection (own it in the thread that waits for the server).
pub struct Endpoint {
    stream: UnixStream,
    /// `endpoint.welcome.v1` as the server sent it (version, methods, capabilities).
    pub welcome: serde_json::Value,
}

/// Writing side; cheap to create from an [`Endpoint`] and safe to use from another thread.
pub struct EndpointSender {
    stream: UnixStream,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

impl Endpoint {
    /// Connects to the server's client socket and completes the endpoint handshake.
    pub fn connect(socket: &Path, geometry: Geometry) -> io::Result<Self> {
        let mut stream = UnixStream::connect(socket)?;
        let hello = serde_json::json!({
            "generation": 1,
            "cell_width_px": geometry.cell_width_px,
            "cell_height_px": geometry.cell_height_px,
            "surface_size": { "cols": geometry.cols, "rows": geometry.rows },
            "pixel_mouse": false,
            "direct_graphics": false,
            "endpoint_keybindings": false,
            "mouse_capture": true,
            "surface_active": true,
            "surface_reuse": false,
            "surface_delta": false,
            "surface_scroll": false,
            "snapshot_codecs": ["shell.snapshot.v1"],
            "surface_codecs": ["shell.surface.v1"],
            "input_codecs": ["shell.input.semantic.v1"],
            "blob_codecs": ["shell.blob.v1"],
        });
        write_frame(
            &mut stream,
            &ClientMessage::EndpointControl {
                kind: HELLO.into(),
                data: hello.to_string(),
            },
        )?;
        loop {
            match parse_server_message(&read_frame(&mut stream)?).map_err(invalid)? {
                Incoming::Control { kind, data } if kind == WELCOME => {
                    let welcome: serde_json::Value =
                        serde_json::from_str(&data).map_err(|e| invalid(e.to_string()))?;
                    if let Some(error) = welcome.get("error").filter(|e| !e.is_null()) {
                        return Err(invalid(format!("the server refused the endpoint: {error}")));
                    }
                    return Ok(Self { stream, welcome });
                }
                Incoming::Welcome {
                    error: Some(error), ..
                } => return Err(invalid(error)),
                _ => {} // anything before the welcome is not ours
            }
        }
    }

    /// Blocks for the next message; messages this app does not use come back as `Incoming::Other`.
    pub fn read(&mut self) -> io::Result<Incoming> {
        parse_server_message(&read_frame(&mut self.stream)?).map_err(invalid)
    }

    /// Like [`read`](Self::read) but gives up after `wait` (`Ok(None)`). Only for tools and tests: a
    /// timeout in the middle of a frame would lose its start, so the app itself reads blocking.
    pub fn read_for(&mut self, wait: std::time::Duration) -> io::Result<Option<Incoming>> {
        self.stream.set_read_timeout(Some(wait))?;
        let result = self.read();
        self.stream.set_read_timeout(None)?;
        match result {
            Ok(message) => Ok(Some(message)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub fn sender(&self) -> io::Result<EndpointSender> {
        Ok(EndpointSender {
            stream: self.stream.try_clone()?,
        })
    }
}

impl EndpointSender {
    /// Closes the connection for both sides, which wakes the thread blocked in [`Endpoint::read`] with an error.
    /// Used to leave a session: the reader then reconnects to wherever the app points to next.
    pub fn close(&self) -> io::Result<()> {
        self.stream.shutdown(std::net::Shutdown::Both)
    }

    pub fn resize(&mut self, geometry: Geometry) -> io::Result<()> {
        write_frame(
            &mut self.stream,
            &ClientMessage::ClientShellResize {
                cell_width_px: geometry.cell_width_px,
                cell_height_px: geometry.cell_height_px,
                surface_size: ClientSurfaceSize {
                    cols: geometry.cols,
                    rows: geometry.rows,
                },
                pixel_mouse: false,
            },
        )
    }

    fn pane_input(&mut self, pane_id: &str, events: Vec<ClientPaneInputEvent>) -> io::Result<()> {
        write_frame(
            &mut self.stream,
            &ClientMessage::ClientShellPaneInput {
                pane_id: pane_id.to_owned(),
                events,
            },
        )
    }

    /// Typed text, as the keyboard produced it.
    pub fn send_text(&mut self, pane_id: &str, text: &str) -> io::Result<()> {
        self.pane_input(
            pane_id,
            vec![ClientPaneInputEvent::TextCommit(text.to_owned())],
        )
    }

    pub fn paste(&mut self, pane_id: &str, text: &str) -> io::Result<()> {
        self.pane_input(pane_id, vec![ClientPaneInputEvent::Paste(text.to_owned())])
    }

    /// A key that is not plain text (Enter, arrows, Ctrl+letter, …).
    pub fn send_key(
        &mut self,
        pane_id: &str,
        code: ClientKeyCode,
        modifiers: u8,
    ) -> io::Result<()> {
        self.pane_input(
            pane_id,
            vec![ClientPaneInputEvent::Key {
                code,
                modifiers,
                kind: ClientKeyKind::Press,
                repeat_count: 1,
                shifted_codepoint: None,
                generated_text: None,
                tracks_release: false,
                physical_key_id: None,
                windows_record: None,
            }],
        )
    }
}

impl EndpointSender {
    /// A mouse event at a cell of the pane (column and row relative to the pane's content).
    pub fn send_mouse(
        &mut self,
        pane_id: &str,
        kind: ClientMouseKind,
        column: u16,
        row: u16,
        modifiers: u8,
        lines: u16,
    ) -> io::Result<()> {
        self.pane_input(
            pane_id,
            vec![ClientPaneInputEvent::Mouse {
                kind,
                position: ClientMousePosition::Cell { column, row },
                geometry: None,
                modifiers,
                lines,
            }],
        )
    }
}

/// The rows of a surface as plain text; used by tests and the `attach` example.
pub fn surface_text(frame: &crate::wire::FrameData) -> String {
    let width = frame.width.max(1) as usize;
    frame
        .cells
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|c| if c.skip { "" } else { c.symbol.as_str() })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_sender_ends_the_reading_side() {
        use std::io::Read;
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let sender = EndpointSender { stream: ours };
        sender.close().unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(theirs.read(&mut byte).unwrap(), 0, "the other end sees EOF");
    }
}
