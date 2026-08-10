use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

use super::MouseProtocolEncoding;

#[allow(dead_code)] // exercised in input unit tests; pane runtime uses backend helpers
pub fn encode_mouse_scroll(
    kind: MouseEventKind,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let button = match kind {
        MouseEventKind::ScrollUp => 64u16,
        MouseEventKind::ScrollDown => 65u16,
        MouseEventKind::ScrollLeft => 66u16,
        MouseEventKind::ScrollRight => 67u16,
        _ => return None,
    };
    encode_mouse_cb(button, false, column, row, modifiers, encoding)
}

#[allow(dead_code)] // exercised in input unit tests; pane runtime uses backend helpers
pub fn encode_mouse_button(
    kind: MouseEventKind,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let (button, release) = match kind {
        MouseEventKind::Down(MouseButton::Left) => (0u16, false),
        MouseEventKind::Down(MouseButton::Middle) => (1u16, false),
        MouseEventKind::Down(MouseButton::Right) => (2u16, false),
        MouseEventKind::Up(MouseButton::Left) => (0u16, true),
        MouseEventKind::Up(MouseButton::Middle) => (1u16, true),
        MouseEventKind::Up(MouseButton::Right) => (2u16, true),
        MouseEventKind::Drag(MouseButton::Left) => (32u16, false),
        MouseEventKind::Drag(MouseButton::Middle) => (33u16, false),
        MouseEventKind::Drag(MouseButton::Right) => (34u16, false),
        _ => return None,
    };
    encode_mouse_cb(button, release, column, row, modifiers, encoding)
}

fn encode_mouse_cb(
    base_button: u16,
    release: bool,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let mut cb = match (encoding, release) {
        (MouseProtocolEncoding::Sgr | MouseProtocolEncoding::SgrPixels, true) => base_button,
        (_, true) => 3,
        (_, false) => base_button,
    };
    if modifiers.contains(KeyModifiers::SHIFT) {
        cb += 4;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        cb += 8;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        cb += 16;
    }

    let column = column as u32 + 1;
    let row = row as u32 + 1;

    match encoding {
        MouseProtocolEncoding::Sgr | MouseProtocolEncoding::SgrPixels => Some(
            format!(
                "\x1b[<{cb};{column};{row}{}",
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        ),
        MouseProtocolEncoding::Default => {
            let cb = u8::try_from(cb + 32).ok()?;
            let column = u8::try_from(column + 32).ok()?;
            let row = u8::try_from(row + 32).ok()?;
            Some(vec![0x1b, b'[', b'M', cb, column, row])
        }
        MouseProtocolEncoding::Utf8 => {
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(b"\x1b[M");
            push_mouse_codepoint(&mut bytes, cb as u32 + 32)?;
            push_mouse_codepoint(&mut bytes, column + 32)?;
            push_mouse_codepoint(&mut bytes, row + 32)?;
            Some(bytes)
        }
    }
}

fn push_mouse_codepoint(bytes: &mut Vec<u8>, value: u32) -> Option<()> {
    let ch = char::from_u32(value)?;
    let mut buf = [0u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_mouse_scroll_encodes_wheel_button_and_coordinates() {
        let encoded = encode_mouse_scroll(
            MouseEventKind::ScrollDown,
            4,
            6,
            KeyModifiers::SHIFT,
            MouseProtocolEncoding::Sgr,
        )
        .expect("mouse scroll should encode");

        assert_eq!(encoded, b"\x1b[<69;5;7M");
    }

    #[test]
    fn sgr_mouse_release_keeps_button_code() {
        let encoded = encode_mouse_button(
            MouseEventKind::Up(MouseButton::Left),
            11,
            9,
            KeyModifiers::empty(),
            MouseProtocolEncoding::Sgr,
        )
        .expect("mouse release should encode");

        assert_eq!(encoded, b"\x1b[<0;12;10m");
    }
}
