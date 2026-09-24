use super::*;
use base64::Engine as _;

fn terminal() -> Terminal {
    let mut terminal = Terminal::new(20, 10, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.resize(20, 10, 8, 16).unwrap();
    terminal
}

#[test]
fn decoded_upload_remains_bytes_backed() {
    let mut terminal = terminal();
    let bytes = [17u8; 16];
    terminal.write(
        format!(
            "\x1b_Ga=T,f=32,s=2,v=2,i=7,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
        .as_bytes(),
    );
    let metadata = terminal
        .kitty_image_placements_with_data_filter(|descriptor| {
            assert!(!descriptor.source_file);
            assert_eq!(descriptor.data_len, bytes.len());
            false
        })
        .unwrap();
    assert_eq!(metadata.len(), 1);
    assert!(metadata[0].data.is_empty());
    assert!(metadata[0].source_file.is_none());
    let compatible = terminal.kitty_image_placements().unwrap();
    assert_eq!(compatible[0].data, bytes);
    assert_eq!(compatible[0].data_fingerprint, metadata[0].data_fingerprint);
}

#[cfg(unix)]
#[test]
fn regular_file_placement_survives_alternate_screen_replacement() {
    let store = crate::pane_graphics_files::FileStore::default();
    let mut terminal = terminal();
    terminal.set_kitty_source_forwarding(false).unwrap();
    terminal.write(b"\x1b[?1049h\x1b[2J\x1b[H");
    for value in [17u8, 99] {
        let bytes = [value; 16];
        let source = store.export(&bytes).unwrap();
        terminal.write(
            format!(
                "\x1b[H\x1b_Ga=T,t=f,f=32,s=2,v=2,i=77,p=1,c=2,r=1,C=1,q=2;{}\x1b\\",
                base64::engine::general_purpose::STANDARD
                    .encode(source.path().as_os_str().as_encoded_bytes())
            )
            .as_bytes(),
        );
        let placements = terminal.kitty_image_placements().unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].data, bytes);
        assert_eq!(placements[0].render.viewport_row, 0);
        assert_eq!(placements[0].render.grid_rows, 1);
        assert_eq!(placements[0].render.grid_cols, 2);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn file_upload_automatically_retains_snapshot_when_platform_supports_clone() {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/tmp".into());
    let path = base.join(format!(
        "herdr-native-source-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = [42u8; 16];
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    use std::io::Write;
    (&file).write_all(&bytes).unwrap();
    // Probe the actual platform, not filesystem names: overlayfs/tmpfs must
    // decline rather than silently produce a userspace copy.
    let probe = crate::pane_graphics_files::FileStore::native_sources()
        .snapshot(i64::from(file.as_raw_fd()), bytes.len());
    let supported = match probe {
        Ok(snapshot) => {
            drop(snapshot);
            true
        }
        Err(error) => {
            eprintln!("native snapshot unsupported on test filesystem: {error}");
            false
        }
    };
    let mut terminal = terminal();
    terminal.write(
        format!(
            "\x1b_Ga=T,t=f,f=32,s=2,v=2,i=7,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(path.as_os_str().as_encoded_bytes())
        )
        .as_bytes(),
    );
    let metadata = terminal
        .kitty_image_placements_with_data_filter(|descriptor| {
            assert_eq!(descriptor.source_file, supported);
            false
        })
        .unwrap();
    assert_eq!(metadata.len(), 1);
    assert!(metadata[0].data.is_empty());
    assert_eq!(metadata[0].source_file.is_some(), supported);
    let mut unicode = self::terminal();
    unicode.write(
        format!(
            "\x1b_Ga=T,t=f,f=32,s=2,v=2,U=1,c=1,r=1,i=8,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(path.as_os_str().as_encoded_bytes())
        )
        .as_bytes(),
    );
    unicode.write("\x1b[H\x1b[38;2;0;0;8m\u{10eeee}\u{0305}\u{0305}\x1b[0m".as_bytes());
    let unicode_metadata = unicode
        .kitty_image_placements_with_data_filter(|descriptor| {
            assert_eq!(descriptor.source_file, supported);
            false
        })
        .unwrap();
    assert_eq!(unicode_metadata.len(), 1);
    assert!(unicode_metadata[0].data.is_empty());
    assert_eq!(unicode_metadata[0].source_file.is_some(), supported);
    std::fs::write(&path, [99u8; 16]).unwrap();
    assert_eq!(unicode.kitty_image_placements().unwrap()[0].data, bytes);
    let compatible = terminal.kitty_image_placements().unwrap();
    assert_eq!(compatible[0].data, bytes);
    assert_eq!(compatible[0].data_fingerprint, metadata[0].data_fingerprint);
    drop(terminal);
    if let Some(source) = &metadata[0].source_file {
        assert_eq!(source.copy_rgba().unwrap(), bytes);
    }
    std::fs::remove_file(path).unwrap();
}
