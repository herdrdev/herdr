use super::*;
use std::io::Write;

pub(super) fn decode_clipboard_payload(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;

    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

pub(super) fn forward_clipboard(data: &str, direct_attach: bool, output: &mut impl Write) -> bool {
    let Some(bytes) = decode_clipboard_payload(data) else {
        warn!("received invalid clipboard payload from server");
        return false;
    };
    if direct_attach {
        write!(output, "\x1b]52;c;{data}\x07").is_ok()
    } else {
        crate::selection::write_osc52_bytes(&bytes)
    }
}
