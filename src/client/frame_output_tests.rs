use super::frame_output::*;
use super::image_files::FileTransport;
use crate::kitty_graphics::{GraphicsOperation, GraphicsOutput};
use std::sync::Arc;

fn output(format: u32) -> GraphicsOutput {
    GraphicsOutput {
        operations: vec![
            GraphicsOperation::Bytes(b"before".to_vec()),
            GraphicsOperation::Upload {
                control: format!("a=t,t=d,f={format},s=1,v=1,i=42,q=2"),
                data: Arc::from([1, 2, 3, 4]),
            },
            GraphicsOperation::Bytes(b"after".to_vec()),
        ],
    }
}

#[test]
fn deferred_inline_output_matches_existing_frame_envelope() {
    let graphics = output(32);
    let encoded = b"\x1b[?2026htext\x1b[?2026l";
    let mut expected = Vec::new();
    write_encoded_frame_with_graphics(
        &mut expected,
        encoded,
        &graphics.clone().into_inline_bytes(),
    )
    .unwrap();
    let mut actual = Vec::new();
    write_composed_frame(
        &mut actual,
        encoded,
        &graphics,
        &mut FileTransport::default(),
    )
    .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn native_swap_cleanup_and_placements_share_the_frame_sync_envelope() {
    let graphics = GraphicsOutput::from_bytes(
        concat!(
            "\x1b_Ga=d,d=I,i=41,q=2;\x1b\\",
            "\x1b_Ga=p,i=42,p=1,q=2;\x1b\\",
            "\x1b_Ga=p,i=42,p=2,q=2;\x1b\\",
        )
        .as_bytes()
        .to_vec(),
    );
    let mut actual = Vec::new();
    write_composed_frame(
        &mut actual,
        b"\x1b[?2026hframe\x1b[?2026l",
        &graphics,
        &mut FileTransport::default(),
    )
    .unwrap();
    let text = std::str::from_utf8(&actual).unwrap();
    let sync_begin = text.find("\x1b[?2026h").unwrap();
    let sync_end = text.rfind("\x1b[?2026l").unwrap();
    let deletion = text.find("a=d,d=I,i=41").unwrap();
    let placements = text
        .match_indices("a=p,i=42")
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    assert!(deletion > sync_begin && deletion < sync_end);
    assert!(placements.len() >= 2);
    assert!(placements
        .iter()
        .all(|placement| *placement > sync_begin && *placement < sync_end));
}

#[test]
fn deferred_output_propagates_write_failure() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        write_composed_frame(Broken, b"text", &output(32), &mut FileTransport::default())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[cfg(unix)]
mod local_files {
    use super::*;
    use base64::Engine as _;
    use std::path::PathBuf;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = PathBuf::from(format!(
                "/var/tmp/herdr-output-test-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn command_path(bytes: &[u8], header: &str) -> PathBuf {
        let text = std::str::from_utf8(bytes).unwrap();
        let command = text
            .split("\x1b_G")
            .find(|part| part.starts_with(header))
            .unwrap();
        let payload = command
            .split_once(';')
            .unwrap()
            .1
            .split("\x1b\\")
            .next()
            .unwrap();
        PathBuf::from(
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .unwrap(),
            )
            .unwrap(),
        )
    }
    #[test]
    fn files_begin_only_after_probe_consumption_and_keep_order() {
        for format in [24, 32, 100] {
            let root = Scratch::new();
            let mut files = FileTransport::for_test(root.0.clone());
            let graphics = output(format);
            let mut first = Vec::new();
            write_composed_frame(&mut first, b"text", &graphics, &mut files).unwrap();
            let first_text = std::str::from_utf8(&first).unwrap();
            assert!(first_text.contains("a=q,t=t"));
            assert!(first_text.contains("a=t,t=d"));
            let probe = command_path(&first, "a=q,");
            assert_eq!(std::fs::read(&probe).unwrap().len(), 4);
            let mut waiting = Vec::new();
            write_composed_frame(&mut waiting, b"text", &graphics, &mut files).unwrap();
            assert!(!std::str::from_utf8(&waiting).unwrap().contains("a=q,"));
            assert!(std::str::from_utf8(&waiting).unwrap().contains("a=t,t=d"));
            std::fs::remove_file(&probe).unwrap();
            let mut ready = Vec::new();
            write_composed_frame(&mut ready, b"text", &graphics, &mut files).unwrap();
            let text = std::str::from_utf8(&ready).unwrap();
            assert!(text.contains(&format!("before\x1b_Ga=t,t=t,f={format}")));
            assert!(text.contains("\x1b\\after"));
            assert!(!text.contains("AQIDBA=="));
            let image = command_path(&ready, "a=t,");
            assert_eq!(std::fs::read(&image).unwrap(), [1, 2, 3, 4]);
            drop(files);
            assert!(!image.exists());
        }
    }
    #[test]
    fn unsupported_format_and_file_creation_failure_remain_inline() {
        let root = Scratch::new();
        let mut files = FileTransport::for_test(root.0.clone());
        let mut unsupported = Vec::new();
        write_composed_frame(&mut unsupported, b"", &output(999), &mut files).unwrap();
        assert!(std::str::from_utf8(&unsupported)
            .unwrap()
            .contains("a=t,t=d,f=999"));
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
        let mut broken = FileTransport::for_test(root.0.join("absent"));
        let mut actual = Vec::new();
        write_composed_frame(&mut actual, b"", &output(32), &mut broken).unwrap();
        let mut expected = Vec::new();
        write_composed_frame(
            &mut expected,
            b"",
            &output(32),
            &mut FileTransport::default(),
        )
        .unwrap();
        assert_eq!(actual, expected);
    }
}
