use std::path::PathBuf;

use tracing::{info, warn};

#[cfg(windows)]
use crate::protocol::ClientInputEvent;
use crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD;
use crate::protocol::{ClientClipboardImageTarget, ClientMessage};

use super::{write_to_server, ClientError};

pub(super) fn write_remote_image_to_server(
    stream: &mut impl super::ClientMessageSink,
    target: ClientClipboardImageTarget,
    image: crate::platform::ClipboardImage,
    source: &'static str,
) -> Result<(), ClientError> {
    if image.bytes.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
        warn!(
            bytes = image.bytes.len(),
            max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
            source,
            "local image is too large to bridge"
        );
        return Ok(());
    }

    info!(
        bytes = image.bytes.len(),
        extension = image.extension,
        source,
        "bridging local image to remote server"
    );
    write_to_server(
        stream,
        &ClientMessage::ClipboardImage {
            target,
            extension: image.extension.to_owned(),
            data: image.bytes,
        },
    )
    .map_err(ClientError::ConnectionLost)
}

pub(super) fn client_remote_image_paste_key(
    config: &crate::config::Config,
) -> Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)> {
    match config.remote_image_paste_key() {
        Ok(key) => key,
        Err(diagnostic) => {
            warn!(diagnostic = %diagnostic, "local remote image paste key config diagnostic");
            None
        }
    }
}

pub(super) fn endpoint_accepts_local_images(
    remote_client_process: bool,
    endpoint_id: &super::endpoint::ClientEndpointId,
    active_surface_available: bool,
) -> bool {
    active_surface_available && (remote_client_process || !endpoint_id.is_local())
}

#[cfg(unix)]
pub(super) fn should_bridge_clipboard_image_paste(
    data: &[u8],
    is_remote_client: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if !is_remote_client {
        return false;
    }
    if data == b"\x1b[200~\x1b[201~" {
        return true;
    }

    let Some(remote_image_paste_key) = remote_image_paste_key else {
        return false;
    };

    let events = crate::raw_input::parse_framed_input(data);
    matches!(
        events.as_slice(),
        [crate::raw_input::RawInputEvent::Key(key)]
            if key.kind == crossterm::event::KeyEventKind::Press
                && crate::config::terminal_key_matches_combo(key, remote_image_paste_key)
    )
}

#[cfg(windows)]
pub(super) fn should_bridge_local_clipboard_image_events(
    events: &[ClientInputEvent],
    local_surface_available: bool,
) -> bool {
    // Reserve Alt+V only when an image can actually be read. Ctrl+V and empty
    // bracketed pastes keep their native meaning in local terminal apps.
    local_surface_available
        && matches!(
            events,
            [event] if matches!(
                event.to_raw_input_event(),
                crate::raw_input::RawInputEvent::Key(key)
                    if key.kind == crossterm::event::KeyEventKind::Press
                        && crate::config::terminal_key_matches_combo(
                            &key,
                            (crossterm::event::KeyCode::Char('v'), crossterm::event::KeyModifiers::ALT),
                        )
            )
        )
}

#[cfg(windows)]
pub(super) fn should_bridge_clipboard_image_events(
    events: &[ClientInputEvent],
    is_remote_client: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if !is_remote_client {
        return false;
    }
    if matches!(events, [ClientInputEvent::Paste { text }] if text.is_empty()) {
        return true;
    }

    let Some(remote_image_paste_key) = remote_image_paste_key else {
        return false;
    };
    matches!(
        events,
        [event]
            if matches!(
                event.to_raw_input_event(),
                crate::raw_input::RawInputEvent::Key(key)
                    if key.kind == crossterm::event::KeyEventKind::Press
                        && crate::config::terminal_key_matches_combo(
                            &key,
                            remote_image_paste_key,
                        )
            )
    )
}

#[cfg(unix)]
pub(super) fn read_image_file_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let (path, extension) = image_path_from_terminal_drop(data, is_remote_client)?;
    read_image_file(path, extension)
}

#[cfg(windows)]
pub(super) fn read_image_file_from_client_events(
    events: &[ClientInputEvent],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let [ClientInputEvent::Paste { text }] = events else {
        return None;
    };
    let text = normalized_terminal_drop_text(text)?;
    let (path, extension) =
        image_path_from_drop_text(strip_matching_path_quotes(text), is_remote_client)?;
    read_image_file(path, extension)
}

fn read_image_file(
    path: PathBuf,
    extension: &'static str,
) -> Option<crate::platform::ClipboardImage> {
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let file = std::fs::File::open(&path).ok()?;
    let bytes =
        match crate::platform::read_limited_reader(file, MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()? {
            crate::platform::LimitedRead::Complete(bytes) => bytes,
            crate::platform::LimitedRead::Empty => return None,
            crate::platform::LimitedRead::Oversized => {
                warn!(
                    max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                    "local image file drop is too large to bridge"
                );
                return None;
            }
        };

    Some(crate::platform::ClipboardImage { bytes, extension })
}

#[cfg(unix)]
pub(super) fn image_path_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<(PathBuf, &'static str)> {
    let bytes = bracketed_paste_payload(data).unwrap_or(data);
    let text = std::str::from_utf8(bytes).ok()?;
    let text = normalized_terminal_drop_text(text)?;
    let text = unescape_terminal_drop_path(strip_matching_path_quotes(text));
    image_path_from_drop_text(&text, is_remote_client)
}

fn normalized_terminal_drop_text(text: &str) -> Option<&str> {
    let text = text.trim_end_matches(['\r', '\n']);
    (!text.is_empty() && !text.contains(['\r', '\n'])).then_some(text)
}

fn image_path_from_drop_text(
    text: &str,
    is_remote_client: bool,
) -> Option<(PathBuf, &'static str)> {
    if !is_remote_client {
        return None;
    }
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return None;
    }
    let extension = recognized_image_extension(path.extension()?.to_str()?)?;
    Some((path, extension))
}

#[cfg(unix)]
fn bracketed_paste_payload(data: &[u8]) -> Option<&[u8]> {
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    data.strip_prefix(START)?.strip_suffix(END)
}

fn strip_matching_path_quotes(text: &str) -> &str {
    if text.len() < 2 {
        return text;
    }

    let bytes = text.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) => &text[1..text.len() - 1],
        _ => text,
    }
}

#[cfg(unix)]
fn unescape_terminal_drop_path(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                unescaped.push(escaped);
            } else {
                unescaped.push(ch);
            }
        } else {
            unescaped.push(ch);
        }
    }
    unescaped
}

fn recognized_image_extension(extension: &str) -> Option<&'static str> {
    if extension.eq_ignore_ascii_case("png") {
        Some("png")
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        Some("jpg")
    } else if extension.eq_ignore_ascii_case("gif") {
        Some("gif")
    } else if extension.eq_ignore_ascii_case("webp") {
        Some("webp")
    } else if extension.eq_ignore_ascii_case("bmp") {
        Some("bmp")
    } else {
        None
    }
}

#[cfg(all(test, windows))]
mod local_image_tests {
    use super::*;
    use crate::protocol::{ClientKeyCode, ClientKeyKind, ClientKeySource};
    use crossterm::event::KeyModifiers;

    fn key(modifiers: KeyModifiers, kind: ClientKeyKind) -> ClientInputEvent {
        ClientInputEvent::Key {
            code: ClientKeyCode::Char('v'),
            modifiers: modifiers.bits(),
            kind,
            repeat_count: 1,
            generated_text: None,
            source: ClientKeySource::Synthesized,
        }
    }

    #[test]
    fn local_image_paste_accepts_alt_v_only_on_available_surface() {
        let events = [key(KeyModifiers::ALT, ClientKeyKind::Press)];
        assert!(should_bridge_local_clipboard_image_events(&events, true));
        assert!(!should_bridge_local_clipboard_image_events(&events, false));
    }

    #[test]
    fn local_image_paste_preserves_text_ctrl_v_and_key_release() {
        for event in [
            key(KeyModifiers::CONTROL, ClientKeyKind::Press),
            key(KeyModifiers::empty(), ClientKeyKind::Press),
            key(KeyModifiers::ALT, ClientKeyKind::Release),
            ClientInputEvent::Paste {
                text: String::new(),
            },
            ClientInputEvent::Paste {
                text: "hello".into(),
            },
        ] {
            assert!(!should_bridge_local_clipboard_image_events(&[event], true));
        }
    }

    #[test]
    fn local_image_paste_does_not_swallow_batched_input() {
        let events = [
            key(KeyModifiers::ALT, ClientKeyKind::Press),
            key(KeyModifiers::empty(), ClientKeyKind::Press),
        ];
        assert!(!should_bridge_local_clipboard_image_events(&events, true));
    }

    #[test]
    #[ignore = "requires an image in the interactive Windows clipboard"]
    fn local_image_paste_reads_real_clipboard_and_stages_png() {
        struct StagingSink(Option<std::path::PathBuf>);
        impl super::super::ClientMessageSink for StagingSink {
            fn send_client_message(&mut self, message: &ClientMessage) -> std::io::Result<()> {
                let ClientMessage::ClipboardImage {
                    target,
                    extension,
                    data,
                } = message
                else {
                    panic!("expected clipboard image message");
                };
                assert!(matches!(target, ClientClipboardImageTarget::Pane(id) if id == "w1:p1"));
                let staged = crate::server::clipboard_image::stage(0, extension, data)?;
                assert_eq!(std::fs::read(&staged.path)?, *data);
                assert_eq!(staged.paste_text, staged.path.to_string_lossy());
                self.0 = Some(staged.path);
                Ok(())
            }
        }
        let events = [key(KeyModifiers::ALT, ClientKeyKind::Press)];
        assert!(should_bridge_local_clipboard_image_events(&events, true));
        let image = crate::platform::read_clipboard_image().expect("copy an image before running");
        assert_eq!(image.extension, "png");
        assert!(image.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let mut sink = StagingSink(None);
        write_remote_image_to_server(
            &mut sink,
            ClientClipboardImageTarget::Pane("w1:p1".into()),
            image,
            "local clipboard test",
        )
        .unwrap();
        std::fs::remove_file(sink.0.expect("image was staged")).unwrap();
    }
}
