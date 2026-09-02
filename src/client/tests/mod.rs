use super::*;
use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn resize_signal_reports_even_when_polled_size_is_unchanged() {
    let size = (120, 40, 8, 16, true);
    assert!(resize_report_required(true, size, size));
    assert!(!resize_report_required(false, size, size));
    assert!(resize_report_required(false, (120, 41, 8, 16, true), size));
    assert!(resize_report_required(false, (120, 40, 9, 18, true), size));
    assert!(resize_report_required(false, (120, 40, 8, 16, false), size));
}

#[test]
fn direct_graphics_profile_is_narrow_and_transport_safe() {
    for (program, term, kitty, expected) in [
        ("ghostty", "", false, true),
        ("WezTerm", "", false, true),
        ("", "xterm-kitty", false, true),
        ("", "xterm-256color", true, true),
        ("", "xterm-256color", false, false),
    ] {
        assert_eq!(
            direct_graphics_profile_values(program, term, kitty, false, true),
            expected
        );
    }
    assert!(!direct_graphics_profile_values(
        "ghostty", "", false, true, true
    ));
    assert!(!direct_graphics_profile_values(
        "ghostty", "", false, false, false
    ));
}

fn restore_env_var(key: &str, value: Option<OsString>) {
    if let Some(value) = value {
        std::env::set_var(key, value);
    } else {
        std::env::remove_var(key);
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        restore_env_var(self.key, self.previous.clone());
    }
}

#[test]
fn windows_virtual_terminal_input_mode_sets_only_vti_bit() {
    assert_eq!(windows_virtual_terminal_input_mode(0x01f0), 0x03f0);
    assert_eq!(windows_virtual_terminal_input_mode(0x03f0), 0x03f0);
}

struct EnvVarsRemovedGuard {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvVarsRemovedGuard {
    fn new(keys: &[&'static str]) -> Self {
        let previous: Vec<_> = keys
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }
        Self { previous }
    }
}

impl Drop for EnvVarsRemovedGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.clone() {
            restore_env_var(key, value);
        }
    }
}

#[test]
fn remote_client_uses_extended_handshake_timeout() {
    let _guard = env_lock().lock().unwrap();
    let _remote = EnvVarGuard::set(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR, "local");

    assert_eq!(handshake_read_timeout(), REMOTE_HANDSHAKE_READ_TIMEOUT);
}

#[test]
fn host_cursor_policy_auto_uses_platform_default() {
    assert_eq!(
        should_draw_host_cursor(crate::config::HostCursorModeConfig::Auto),
        crate::platform::should_draw_host_cursor_by_default()
    );
}

#[test]
fn host_cursor_policy_native_and_drawn_override_auto_detection() {
    let _guard = env_lock().lock().unwrap();
    let _env = EnvVarGuard::set("TERM_PROGRAM", "WezTerm");

    assert!(!should_draw_host_cursor(
        crate::config::HostCursorModeConfig::Native
    ));
    assert!(should_draw_host_cursor(
        crate::config::HostCursorModeConfig::Drawn
    ));
}

#[cfg(unix)]
#[test]
fn clipboard_image_paste_bridge_triggers_on_configured_key_and_empty_paste() {
    let ctrl_v = crate::config::parse_key_combo("ctrl+v").unwrap();
    assert!(should_bridge_clipboard_image_paste(
        &[0x16],
        true,
        Some(ctrl_v)
    ));
    assert!(should_bridge_clipboard_image_paste(
        b"\x1b[118;5u",
        true,
        Some(ctrl_v)
    ));
    assert!(should_bridge_clipboard_image_paste(
        b"\x1b[200~\x1b[201~",
        true,
        None
    ));
    assert!(!should_bridge_clipboard_image_paste(
        b"\x1b[200~\x1b[201~",
        false,
        Some(ctrl_v)
    ));
    assert!(!should_bridge_clipboard_image_paste(
        b"\x1b[200~text\x1b[201~",
        true,
        Some(ctrl_v)
    ));
    assert!(!should_bridge_clipboard_image_paste(&[0x16], true, None));
    assert!(!should_bridge_clipboard_image_paste(
        b"v",
        true,
        Some(ctrl_v)
    ));
}

struct TempImageFile {
    path: std::path::PathBuf,
}

impl TempImageFile {
    fn new(extension: &str, bytes: &[u8]) -> Self {
        Self::with_name_fragment("test", extension, bytes)
    }

    fn with_name_fragment(name_fragment: &str, extension: &str, bytes: &[u8]) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "herdr-client-drop-{name_fragment}-{}-{nanos}.{extension}",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        Self { path }
    }
}

impl Drop for TempImageFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
#[cfg(unix)]
#[test]
fn remote_image_file_drop_bridge_reads_bracketed_absolute_image_path() {
    let file = TempImageFile::new("PNG", b"image-bytes");
    let input = format!("\x1b[200~{}\x1b[201~", file.path.display());

    let image = read_image_file_from_terminal_drop(input.as_bytes(), true).unwrap();

    assert_eq!(image.extension, "png");
    assert_eq!(image.bytes, b"image-bytes");
}

#[cfg(unix)]
#[test]
fn remote_image_file_drop_bridge_reads_plain_quoted_path_with_newline() {
    let file = TempImageFile::new("jpeg", b"jpeg-bytes");
    let input = format!("'{}'\n", file.path.display());

    let image = read_image_file_from_terminal_drop(input.as_bytes(), true).unwrap();

    assert_eq!(image.extension, "jpg");
    assert_eq!(image.bytes, b"jpeg-bytes");
}

#[cfg(unix)]
#[test]
fn remote_image_file_drop_bridge_unescapes_spaces_in_paths() {
    let file = TempImageFile::with_name_fragment("space test", "png", b"image-bytes");
    let escaped_path = file.path.display().to_string().replace(' ', "\\ ");

    let image = read_image_file_from_terminal_drop(escaped_path.as_bytes(), true).unwrap();

    assert_eq!(image.extension, "png");
    assert_eq!(image.bytes, b"image-bytes");
}

#[cfg(unix)]
#[test]
fn remote_image_file_drop_bridge_ignores_non_remote_and_non_image_input() {
    let file = TempImageFile::new("png", b"image-bytes");
    let path = file.path.display().to_string();

    assert!(read_image_file_from_terminal_drop(path.as_bytes(), false).is_none());
    assert!(read_image_file_from_terminal_drop(b"relative.png\n", true).is_none());
    assert!(read_image_file_from_terminal_drop(b"/tmp/file.txt\n", true).is_none());
    assert!(read_image_file_from_terminal_drop(
        format!("{}\nextra", file.path.display()).as_bytes(),
        true
    )
    .is_none());
}

#[test]
fn graphics_bytes_are_written_inside_synchronized_blit_with_saved_cursor() {
    let mut output = Vec::new();
    write_encoded_frame_with_graphics(
        &mut output,
        b"\x1b[?2026htext\x1b[?2026lcursor",
        b"graphics",
    )
    .unwrap();

    assert_eq!(
        output,
        b"\x1b[?2026htext\x1b7graphics\x1b8\x1b[?2026lcursor"
    );
}

#[test]
fn empty_graphics_writes_only_blit_frame() {
    let mut output = Vec::new();
    write_encoded_frame_with_graphics(&mut output, b"text", b"").unwrap();

    assert_eq!(output, b"text");
}

#[test]
fn terminal_frame_kitty_detection_matches_apc_prefix() {
    assert!(contains_kitty_graphics_bytes(b"text\x1b_Ga=p;\x1b\\"));
    assert!(!contains_kitty_graphics_bytes(b"text\x1b[?2026h"));
}

#[test]
fn kitty_graphics_image_id_parser_tracks_herdr_ids_only() {
    let ids = kitty_graphics_image_ids(
        b"text\x1b_Ga=t,t=d,f=32,s=1,v=1,i=10023,q=2;AAAA\x1b\\\x1b_Ga=p,i=10023,p=7;\x1b\\",
    );
    assert_eq!(ids, vec![10023, 10023]);
}

#[test]
fn kitty_graphics_cleanup_deletes_tracked_images_not_all_images() {
    record_received_kitty_graphics(b"\x1b_Ga=t,i=123,q=2;AAAA\x1b\\");
    let mut output = Vec::new();
    clear_received_kitty_graphics(&mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("a=d,d=I,i=123"));
    assert!(!text.contains("d=A"));
}

#[test]
fn write_host_terminal_appearance_query_emits_mode_2031_query() {
    let mut output = Vec::new();
    write_host_terminal_appearance_query(&mut output).unwrap();
    assert_eq!(output, b"\x1b[?996n");
}

#[test]
fn write_host_terminal_theme_query_emits_osc_queries() {
    let mut output = Vec::new();
    write_host_terminal_theme_query(&mut output).unwrap();
    assert_eq!(
        output,
        crate::terminal_theme::host_terminal_theme_query_sequence(
            crate::platform::should_query_host_terminal_palette(),
        )
        .as_bytes()
    );
    assert!(
        !output
            .windows(crate::terminal_theme::HOST_COLOR_SCHEME_QUERY_SEQUENCE.len())
            .any(|window| window
                == crate::terminal_theme::HOST_COLOR_SCHEME_QUERY_SEQUENCE.as_bytes())
    );
}

#[test]
fn write_host_color_scheme_report_mode_emits_mode_sequences() {
    let mut output = Vec::new();
    write_host_color_scheme_report_mode(&mut output, true).unwrap();
    write_host_color_scheme_report_mode(&mut output, false).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE.as_bytes(),
    );
    expected.extend_from_slice(
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
    );
    assert_eq!(output, expected);
}

#[test]
fn color_scheme_change_event_requests_host_theme_query() {
    let events = crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[?997;1n");

    assert!(crate::raw_input::events_require_host_terminal_theme_query(
        &events
    ));
}

#[test]
fn host_terminal_theme_query_is_disabled_on_windows() {
    assert_eq!(should_query_host_terminal_theme(), !cfg!(windows));
}

#[test]
fn write_host_cell_size_query_emits_xtwinops_request() {
    let mut output = Vec::new();
    write_host_cell_size_query(&mut output).unwrap();

    assert_eq!(output, b"\x1b[16t");
}

#[test]
fn host_cell_size_query_is_disabled_on_windows() {
    assert_eq!(should_query_host_cell_size(), !cfg!(windows));
}

#[test]
fn cell_size_fallback_prefers_reported_then_previous_size() {
    assert_eq!(cell_size_fallback(0, None), (8, 16));
    assert_eq!(cell_size_fallback(0, Some((11, 22))), (11, 22));
    assert_eq!(
        cell_size_fallback(pack_cell_size(10, 21), Some((11, 22))),
        (10, 21)
    );
    assert_eq!(cell_size_fallback(pack_cell_size(10, 0), None), (8, 16));
    assert_eq!(cell_size_fallback(pack_cell_size(0, 21), None), (8, 16));
}

#[test]
fn reported_cell_size_is_taken_from_host_cell_size_events() {
    let events = crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[?997;1n");
    assert_eq!(
        super::terminal_geometry::reported_cell_size_from_events(&events),
        None
    );

    let events = crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[6;21;10t\x1b[6;18;9t");
    assert_eq!(
        super::terminal_geometry::reported_cell_size_from_events(&events),
        Some((9, 18))
    );
}

#[test]
fn color_scheme_reports_are_enabled_only_for_full_clients() {
    assert_eq!(
        should_enable_host_color_scheme_reports(true),
        !cfg!(windows)
    );
    assert!(!should_enable_host_color_scheme_reports(false));
}

#[test]
fn terminal_restore_postlude_restores_visible_default_cursor() {
    let mut output = Vec::new();
    write_terminal_restore_postlude(&mut output, false).unwrap();
    assert_eq!(output, b"\x1b[?25h\x1b[0 q");
}

#[test]
fn direct_attach_mouse_capture_combines_local_preference_with_child_demand() {
    assert!(effective_mouse_capture(false, true));
    assert!(effective_mouse_capture(true, false));
    assert!(!effective_mouse_capture(false, false));
    assert!(effective_sgr_pixel_mouse(true, true, true));
    assert!(!effective_sgr_pixel_mouse(true, true, false));
}

#[test]
fn terminal_restore_postlude_disables_color_scheme_reports_when_enabled() {
    let mut output = Vec::new();
    write_terminal_restore_postlude(&mut output, true).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
    );
    expected.extend_from_slice(b"\x1b[?25h\x1b[0 q");
    assert_eq!(output, expected);
}

#[test]
fn client_error_display_connection_failed() {
    let err = ClientError::ConnectionFailed(io::Error::new(
        io::ErrorKind::ConnectionRefused,
        "connection refused",
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("failed to connect to server"),
        "should mention connection failure: {msg}"
    );
    assert!(
        msg.contains("herdr server"),
        "should suggest starting server: {msg}"
    );
}

#[test]
fn client_error_display_handshake_rejected() {
    let err = ClientError::HandshakeRejected {
        version: 1,
        error: "incompatible".into(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("rejected handshake"),
        "should mention rejection: {msg}"
    );
    assert!(msg.contains("incompatible"), "should include error: {msg}");
}

#[test]
fn client_error_display_server_shutdown() {
    let err = ClientError::ServerShutdown {
        reason: Some("maintenance".into()),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("server shut down"),
        "should mention shutdown: {msg}"
    );
    assert!(msg.contains("maintenance"), "should include reason: {msg}");
}

#[test]
fn client_error_display_server_shutdown_no_reason() {
    let err = ClientError::ServerShutdown { reason: None };
    let msg = err.to_string();
    assert!(
        msg.contains("server shut down"),
        "should mention shutdown: {msg}"
    );
}

#[test]
fn client_error_display_detached_default_session_reattach_hint() {
    let _guard = env_lock().lock().unwrap();
    let _env = EnvVarsRemovedGuard::new(&[
        crate::remote::REATTACH_COMMAND_ENV_VAR,
        crate::session::SESSION_ENV_VAR,
    ]);
    let err = ClientError::ServerShutdown {
        reason: Some("detached".into()),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("Run `herdr` to reattach"),
        "should suggest default reattach command: {msg}"
    );
}

#[test]
fn client_error_display_detached_named_session_reattach_hint() {
    let _guard = env_lock().lock().unwrap();
    let _remote_env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
    let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
    let err = ClientError::ServerShutdown {
        reason: Some("detached".into()),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("Run `herdr session attach work` to reattach"),
        "should suggest named session reattach command: {msg}"
    );
}

#[test]
fn client_error_display_detached_remote_reattach_hint_takes_precedence() {
    let _guard = env_lock().lock().unwrap();
    let _remote_env = EnvVarGuard::set(
        crate::remote::REATTACH_COMMAND_ENV_VAR,
        "herdr --remote host --session work",
    );
    let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
    let err = ClientError::ServerShutdown {
        reason: Some("detached".into()),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("Run `herdr --remote host --session work` to reattach"),
        "should prefer remote reattach command: {msg}"
    );
}

#[test]
fn client_error_display_connection_lost() {
    let _guard = env_lock().lock().unwrap();
    let _env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
    let err = ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
    let msg = err.to_string();
    assert!(
        msg.contains("lost connection to server"),
        "should mention lost connection: {msg}"
    );
}

#[test]
fn client_error_display_remote_connection_lost_has_reattach_hint() {
    let _guard = env_lock().lock().unwrap();
    let _remote_env = EnvVarGuard::set(
        crate::remote::REATTACH_COMMAND_ENV_VAR,
        "herdr --remote host --session work",
    );
    let err = ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
    let msg = err.to_string();
    assert!(
        msg.contains("lost connection to remote Herdr"),
        "should mention remote connection loss: {msg}"
    );
    assert!(
        msg.contains("panes may still be running"),
        "should explain possible persistence: {msg}"
    );
    assert!(
        msg.contains("Run `herdr --remote host --session work` to reattach"),
        "should show remote reattach command: {msg}"
    );
}

#[test]
fn sound_from_notify_message_maps_done() {
    assert_eq!(
        sound_from_notify_message("agent done"),
        Some(crate::sound::Sound::Done)
    );
}

#[test]
fn sound_from_notify_message_maps_attention() {
    assert_eq!(
        sound_from_notify_message("agent attention"),
        Some(crate::sound::Sound::Request)
    );
}

#[test]
fn sound_from_notify_message_rejects_unknown_payloads() {
    assert_eq!(sound_from_notify_message("toast"), None);
}

#[test]
fn reload_local_client_config_refreshes_local_client_presentation_state() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let path = std::env::temp_dir().join(format!(
        "herdr-client-config-reload-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(
        &path,
        "[ui]\nredraw_on_focus_gained = false\nhost_cursor = \"drawn\"\nmouse_capture = false\n",
    )
    .unwrap();
    let path_string = path.to_string_lossy().to_string();
    let _env = EnvVarGuard::set(crate::config::CONFIG_PATH_ENV_VAR, &path_string);
    let mut sound_config = crate::config::SoundConfig::default();
    let mut redraw_on_focus_gained = true;
    let mut draw_host_cursor = false;
    let mut remote_image_paste_key = None;
    let mut mouse_capture = true;

    reload_local_client_config(
        &mut sound_config,
        &mut redraw_on_focus_gained,
        &mut draw_host_cursor,
        &mut remote_image_paste_key,
        &mut mouse_capture,
    );

    assert!(!redraw_on_focus_gained);
    assert!(draw_host_cursor);
    assert!(!mouse_capture);
    let _ = std::fs::remove_file(path);
}

#[test]
fn reload_local_client_config_keeps_ui_preferences_when_ui_is_invalid() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let path = std::env::temp_dir().join(format!(
        "herdr-client-invalid-ui-reload-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "[ui]\nmouse_capture = \"invalid\"\n").unwrap();
    let path_string = path.to_string_lossy().to_string();
    let _env = EnvVarGuard::set(crate::config::CONFIG_PATH_ENV_VAR, &path_string);
    let mut sound_config = crate::config::SoundConfig::default();
    let mut redraw_on_focus_gained = false;
    let mut draw_host_cursor = true;
    let mut remote_image_paste_key = None;
    let mut mouse_capture = false;

    reload_local_client_config(
        &mut sound_config,
        &mut redraw_on_focus_gained,
        &mut draw_host_cursor,
        &mut remote_image_paste_key,
        &mut mouse_capture,
    );

    assert!(!mouse_capture);
    assert!(!redraw_on_focus_gained);
    assert!(draw_host_cursor);
    let _ = std::fs::remove_file(path);
}

#[test]
fn toast_notify_from_server_is_emitted_even_when_attach_config_was_off() {
    let sound_config = crate::config::SoundConfig::default();
    let mut emitted = None;

    handle_notify_with_notifiers(
        NotifyKind::Toast,
        "pi finished",
        Some("workspace 1"),
        &sound_config,
        |title, body| {
            emitted = Some((title.to_string(), body.map(str::to_string)));
            Ok(true)
        },
        |_, _| Ok(false),
    );

    assert_eq!(
        emitted,
        Some(("pi finished".to_string(), Some("workspace 1".to_string())))
    );
}

#[test]
fn system_toast_notify_from_server_uses_system_notifier() {
    let sound_config = crate::config::SoundConfig::default();
    let mut emitted = None;

    handle_notify_with_notifiers(
        NotifyKind::SystemToast,
        "pi finished",
        Some("workspace 1"),
        &sound_config,
        |_, _| Ok(false),
        |title, body| {
            emitted = Some((title.to_string(), body.map(str::to_string)));
            Ok(true)
        },
    );

    assert_eq!(
        emitted,
        Some(("pi finished".to_string(), Some("workspace 1".to_string())))
    );
}

#[test]
fn system_toast_notify_preserves_colon_in_title() {
    let sound_config = crate::config::SoundConfig::default();
    let mut emitted = None;

    handle_notify_with_notifiers(
        NotifyKind::SystemToast,
        "build: failed",
        Some("api workspace"),
        &sound_config,
        |_, _| Ok(false),
        |title, body| {
            emitted = Some((title.to_string(), body.map(str::to_string)));
            Ok(true)
        },
    );

    assert_eq!(
        emitted,
        Some((
            "build: failed".to_string(),
            Some("api workspace".to_string())
        ))
    );
}

#[test]
fn decode_clipboard_payload_decodes_base64() {
    assert_eq!(decode_clipboard_payload("dGVzdA=="), Some(b"test".to_vec()));
}

#[test]
fn ioctl_cell_size_accepts_fractional_terminal_geometry() {
    assert_eq!(ioctl_cell_size(80, 24, 800, 480), Some((10, 20)));
    assert_eq!(ioctl_cell_size(80, 24, 805, 480), Some((10, 20)));
    assert_eq!(ioctl_cell_size(80, 24, 800, 485), Some((10, 20)));
    assert_eq!(ioctl_cell_size(80, 24, 0, 485), None);
}

#[test]
fn decode_clipboard_payload_rejects_invalid_base64() {
    assert_eq!(decode_clipboard_payload("not-base64!!!"), None);
}

#[test]
fn terminal_control_input_command_accepts_text() {
    let action =
        terminal_control_command_from_json(r#"{"type":"terminal.input","text":"hello"}"#).unwrap();
    let ClientMessage::Input { data } = action else {
        panic!("expected input command");
    };
    assert_eq!(data, b"hello");
}

#[test]
fn terminal_control_input_command_accepts_base64_bytes() {
    let action =
        terminal_control_command_from_json(r#"{"type":"terminal.input","bytes":"G1tB"}"#).unwrap();
    let ClientMessage::Input { data } = action else {
        panic!("expected input command");
    };
    assert_eq!(data, b"\x1b[A");
}

#[test]
fn terminal_control_resize_command_maps_to_client_resize() {
    let action = terminal_control_command_from_json(
        r#"{"type":"terminal.resize","cols":100,"rows":30,"cell_width_px":8,"cell_height_px":16}"#,
    )
    .unwrap();
    let ClientMessage::Resize {
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        pixel_mouse,
    } = action
    else {
        panic!("expected resize command");
    };
    assert_eq!(
        (cols, rows, cell_width_px, cell_height_px),
        (100, 30, 8, 16)
    );
    assert!(!pixel_mouse);
}

#[test]
fn terminal_control_scroll_command_maps_to_attach_scroll() {
    let action = terminal_control_command_from_json(
        r#"{"type":"terminal.scroll","direction":"up","lines":3}"#,
    )
    .unwrap();
    let ClientMessage::AttachScroll {
        source,
        direction,
        lines,
        ..
    } = action
    else {
        panic!("expected scroll command");
    };
    assert_eq!(source, AttachScrollSource::Wheel);
    assert_eq!(direction, AttachScrollDirection::Up);
    assert_eq!(lines, 3);
}

#[test]
fn forward_clipboard_uses_local_clipboard_path() {
    unsafe {
        std::env::set_var("SSH_CONNECTION", "1 2 3 4");
    }
    assert!(forward_clipboard("dGVzdA=="));
    assert!(!forward_clipboard("not base64"));
    unsafe {
        std::env::remove_var("SSH_CONNECTION");
    }
}
