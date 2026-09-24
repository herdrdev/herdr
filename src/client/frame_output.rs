use std::collections::HashSet;
use std::io::{self, Write as _};
use std::sync::{Mutex, OnceLock};

use crate::kitty_graphics::{GraphicsOperation, GraphicsOutput};
use crate::protocol::{render_ansi, FrameData};
use base64::Engine as _;

/// Local output only; raw uploads are never added to a published frame codec.
#[derive(Debug)]
pub(crate) struct ComposedFrame {
    pub(crate) frame: FrameData,
    pub(crate) graphics: GraphicsOutput,
}

impl From<FrameData> for ComposedFrame {
    fn from(mut frame: FrameData) -> Self {
        let graphics = GraphicsOutput::from_bytes(std::mem::take(&mut frame.graphics));
        Self { frame, graphics }
    }
}

impl std::ops::Deref for ComposedFrame {
    type Target = FrameData;

    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}

static RECEIVED_KITTY_GRAPHICS_IDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

pub(super) fn write_composed_frame(
    mut writer: impl io::Write,
    encoded: &[u8],
    graphics: &GraphicsOutput,
    files: &mut super::image_files::FileTransport,
) -> io::Result<()> {
    if graphics.is_empty() {
        return writer.write_all(encoded);
    }
    let mut writer = io::BufWriter::with_capacity(64 * 1024, writer);
    let insertion = render_ansi::final_sync_output_end(encoded).unwrap_or(encoded.len());
    writer.write_all(&encoded[..insertion])?;
    writer.write_all(b"\x1b7")?;
    for operation in &graphics.operations {
        match operation {
            GraphicsOperation::Bytes(bytes) => {
                record_received_kitty_graphics(bytes);
                writer.write_all(bytes)?;
            }
            GraphicsOperation::Upload { control, data } => {
                let file_eligible = control
                    .split(',')
                    .any(|part| matches!(part, "f=24" | "f=32" | "f=100"));
                if file_eligible {
                    if let Some(path) = files
                        .probe()
                        .and_then(|path| path.to_str().map(str::to_owned))
                    {
                        let path =
                            base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
                        write!(writer, "\x1b_Ga=q,t=t,f=32,s=1,v=1,i=1,q=2;{path}\x1b\\")?;
                    }
                }
                let path = file_eligible.then(|| files.prepare(data)).flatten();
                let header = format!("\x1b_G{control};\x1b\\");
                record_received_kitty_graphics(header.as_bytes());
                if let Some(path) = path.as_ref().and_then(|path| path.to_str()) {
                    let control = control.replace(",t=d,", ",t=t,");
                    let path = base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
                    write!(writer, "\x1b_G{control};{path}\x1b\\")?;
                } else {
                    crate::kitty_graphics::write_kitty_data(&mut writer, control, data)?;
                }
            }
        }
    }
    writer.write_all(b"\x1b8")?;
    writer.write_all(&encoded[insertion..])?;
    io::Write::flush(&mut writer)
}

pub(super) fn write_encoded_frame_with_graphics(
    mut writer: impl io::Write,
    encoded: &[u8],
    graphics: &[u8],
) -> io::Result<()> {
    if graphics.is_empty() {
        return writer.write_all(encoded);
    }

    let insertion = render_ansi::final_sync_output_end(encoded).unwrap_or(encoded.len());

    writer.write_all(&encoded[..insertion])?;
    record_received_kitty_graphics(graphics);
    writer.write_all(b"\x1b7")?;
    writer.write_all(graphics)?;
    writer.write_all(b"\x1b8")?;
    writer.write_all(&encoded[insertion..])
}

pub(super) fn contains_kitty_graphics_bytes(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|window| window == b"\x1b_G")
}

pub(super) fn record_received_kitty_graphics(bytes: &[u8]) {
    let ids = kitty_graphics_image_ids(bytes);
    if ids.is_empty() {
        return;
    }
    let set = RECEIVED_KITTY_GRAPHICS_IDS.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut set) = set.lock() {
        set.extend(ids);
    }
}

pub(super) fn clear_received_kitty_graphics(mut writer: impl io::Write) -> io::Result<()> {
    let Some(set) = RECEIVED_KITTY_GRAPHICS_IDS.get() else {
        return Ok(());
    };
    let Ok(mut set) = set.lock() else {
        return Ok(());
    };
    for id in set.drain() {
        write!(writer, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
    }
    writer.flush()
}

pub(super) fn kitty_graphics_image_ids(bytes: &[u8]) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut index = 0usize;
    while let Some(start) = find_subslice(&bytes[index..], b"\x1b_G") {
        let command_start = index + start + 3;
        let Some(end) = find_subslice(&bytes[command_start..], b"\x1b\\") else {
            break;
        };
        let command = &bytes[command_start..command_start + end];
        if let Some(id) = kitty_graphics_command_image_id(command) {
            ids.push(id);
        }
        index = command_start + end + 2;
    }
    ids
}

fn kitty_graphics_command_image_id(command: &[u8]) -> Option<u32> {
    let header_end = command
        .iter()
        .position(|byte| *byte == b';')
        .unwrap_or(command.len());
    for part in command[..header_end].split(|byte| *byte == b',') {
        let Some(value) = part.strip_prefix(b"i=") else {
            continue;
        };
        let text = std::str::from_utf8(value).ok()?;
        if let Ok(id) = text.parse::<u32>() {
            return Some(id);
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
