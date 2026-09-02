//! Thin client mode — connects to the server's client socket.
//!
//! The client:
//! - Connects to `herdr-client.sock`, sends TerminalHello with terminal size and protocol version
//! - Sets up the real terminal (raw mode, mouse capture, keyboard enhancements)
//! - Receives Frame messages and blits them to the terminal (diff against last frame)
//! - Reads stdin events (keystrokes, mouse, paste) and sends them as ClientMessage::Input
//! - Detects terminal resize and sends ClientMessage::Resize
//! - Restores terminal on exit (normal or error)
//! - Handles ServerShutdown gracefully (clean exit, informative message to stderr)
//! - Handles server unreachable (clear error screen, not blank/hang)
//! - Forwards OSC 52 clipboard writes from server to its own stdout
//! - Displays sound/toast notifications forwarded from server

mod attach;
mod clipboard_images;
#[cfg(unix)]
mod direct_graphics;
mod endpoint_commands;
mod errors;
mod frame_output;
mod handshake;
mod input;
mod notifications;
mod shell;
mod terminal_geometry;
mod terminal_sessions;
mod terminal_setup;
mod timer;

#[cfg(test)]
pub(crate) use shell::{ClientShellConfig, ClientShellState};
pub use terminal_sessions::{run_terminal_session_control, run_terminal_session_observe};

#[cfg(not(windows))]
use terminal_geometry::query_host_terminal_appearance;
#[cfg(test)]
use terminal_geometry::{
    cell_size_fallback, ioctl_cell_size, pack_cell_size, resize_report_required,
    should_query_host_cell_size, write_host_cell_size_query, write_host_terminal_appearance_query,
    write_host_terminal_theme_query,
};
use terminal_geometry::{
    host_cell_size_query_required, initial_terminal_geometry, query_host_cell_size,
    query_host_terminal_theme, resize_poll_loop, should_query_host_terminal_theme,
};
#[cfg(unix)]
use terminal_geometry::{reported_cell_size_from_events, store_reported_cell_size};
use terminal_setup::{
    effective_mouse_capture, effective_sgr_pixel_mouse, set_mouse_capture,
    setup_direct_attach_terminal, setup_terminal, should_draw_host_cursor,
};
#[cfg(windows)]
use terminal_setup::{
    enable_windows_virtual_terminal_input, is_ssh_session, windows_vti_input_backend_enabled,
};
#[cfg(test)]
use terminal_setup::{
    should_enable_host_color_scheme_reports, windows_virtual_terminal_input_mode,
    write_host_color_scheme_report_mode, write_terminal_restore_postlude,
};

#[cfg(unix)]
use attach::direct_attach_pixel_mouse;
use attach::AttachEscapeState;
#[cfg(unix)]
use attach::{write_attach_semantic_action, AttachInputAction};
use clipboard_images::{client_remote_image_paste_key, write_remote_image_to_server};
#[cfg(windows)]
use clipboard_images::{read_image_file_from_client_events, should_bridge_clipboard_image_events};
#[cfg(unix)]
use clipboard_images::{read_image_file_from_terminal_drop, should_bridge_clipboard_image_paste};
pub use errors::ClientError;
#[cfg(test)]
use frame_output::{clear_received_kitty_graphics, kitty_graphics_image_ids};
use frame_output::{
    contains_kitty_graphics_bytes, record_received_kitty_graphics,
    write_encoded_frame_with_graphics,
};
use handshake::{client_shell_keybinding_source, do_handshake, is_remote_client_process};
#[cfg(test)]
use handshake::{
    direct_graphics_profile_values, handshake_read_timeout, REMOTE_HANDSHAKE_READ_TIMEOUT,
};
use notifications::{handle_notify, handle_shell_notification_effects};
#[cfg(test)]
use notifications::{handle_notify_with_notifiers, sound_from_notify_message};
#[cfg(test)]
use terminal_sessions::terminal_control_command_from_json;

#[cfg(unix)]
use std::collections::HashMap;
use std::io::{self, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;
use tracing::{debug, info, warn};

use crate::ipc::LocalStream;
use crate::protocol::render_ansi;
use crate::protocol::{
    self, ClientMessage, FrameData, RenderEncoding, ServerMessage, MAX_FRAME_SIZE,
    MAX_GRAPHICS_FRAME_SIZE,
};
#[cfg(test)]
use crate::protocol::{AttachScrollDirection, AttachScrollSource, NotifyKind};
use crate::server::socket_paths::client_socket_path;

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

struct ClientLoopConfig {
    sound_config: crate::config::SoundConfig,
    mouse_scroll_lines: usize,
    redraw_on_focus_gained: bool,
    host_cursor: crate::config::HostCursorModeConfig,
    kitty_graphics_enabled: bool,
    pixel_geometry_enabled: bool,
    pixel_geometry_fallback: bool,
    mouse_capture_active: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    shell_config: Option<shell::ClientShellConfig>,
}

/// State tracking for the thin client.
struct ClientState {
    /// Stateful semantic-frame encoder used when the server sends FrameData.
    blit_encoder: render_ansi::BlitEncoder,
    /// Whether host mouse capture is currently active.
    mouse_capture_active: bool,
    /// Last mouse-capture demand published by the endpoint.
    endpoint_mouse_capture_requested: bool,
    /// Last exact-pixel mouse demand published by the endpoint.
    endpoint_sgr_pixels_requested: bool,
    /// Client-local direct-attach preference, combined with child mouse demand.
    direct_mouse_capture_preference: bool,
    /// Client-owned shell mouse-capture preference.
    shell_mouse_capture_preference: bool,
    /// Host keyboard protocol state currently owned for a direct terminal attach.
    direct_keyboard_protocol: crate::terminal_modes::DirectHostKeyboardState,
    /// Focused endpoint pane/popup demand for Kitty report-all input.
    pane_keyboard_report_all: bool,
    /// Whether the client-owned shell currently enabled host report-all input.
    keyboard_report_all_active: bool,
    /// The terminal size we reported to the server in our last handshake/Resize.
    reported_size: (u16, u16),
    /// Last exact host cell size used by client-rendered surfaces.
    reported_cell_size: (u32, u32),
    /// Client-local sound playback config, refreshed on server request.
    sound_config: crate::config::SoundConfig,
    /// Whether this client may write Kitty graphics bytes to its host terminal.
    kitty_graphics_enabled: bool,
    /// Whether resize reports inspect host pixel geometry.
    pixel_geometry_enabled: bool,
    /// Whether the latest host pixel geometry is exact enough for pixel mouse input.
    pixel_geometry_exact: bool,
    /// One bounded matcher, inactive unless a direct transmission is armed.
    #[cfg(unix)]
    direct_graphics_response: Arc<Mutex<direct_graphics::ResponseMatcher>>,
    /// One server-retired direct transfer to suppress if it was still queued.
    #[cfg(unix)]
    retired_direct_graphics: Option<(u64, u32)>,
    /// ClientShell assets waiting for the host terminal's direct-upload response.
    #[cfg(unix)]
    pending_surface_graphics: HashMap<u64, crate::protocol::SurfaceGraphicsAssetKey>,
    /// Direct attach prefix escape state. None for ClientShell connections.
    attach_escape: Option<AttachEscapeState>,
    /// Rows scrolled for one direct-attach wheel notch.
    #[cfg(unix)]
    mouse_scroll_lines: usize,
    /// Local-client shortcut that sends a clipboard image to a remote Herdr session.
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    /// Whether outer focus gain should force a full host-terminal redraw.
    redraw_on_focus_gained: bool,
    /// Whether the next semantic frame must repaint every cell without clearing the surface.
    repaint_pending: bool,
    /// Whether this client draws the cursor into frame cells instead of using the host cursor.
    draw_host_cursor: bool,
    /// Browser opener processes launched by client-owned link activation.
    detached_process_children: Vec<std::process::Child>,
    /// Experimental client-owned shell state.
    shell: Option<shell::ClientShellState>,
}

impl Drop for ClientState {
    fn drop(&mut self) {
        if self.attach_escape.is_some() {
            let _ = crate::terminal_modes::set_direct_host_keyboard_protocol(
                &mut io::stdout(),
                &mut self.direct_keyboard_protocol,
                0,
                0,
            );
        }
    }
}

impl ClientState {
    fn request_repaint(&mut self) {
        self.repaint_pending = true;
    }

    fn present_graphics(&mut self, graphics: &[u8]) {
        if graphics.is_empty() || !self.kitty_graphics_enabled {
            return;
        }
        let mut stdout = io::stdout();
        let _ = write_encoded_frame_with_graphics(&mut stdout, &[], graphics);
        let _ = stdout.flush();
    }

    fn present_surface_patch(
        &mut self,
        patch: shell::ClientComposedSurfacePatch,
    ) -> io::Result<bool> {
        if self.repaint_pending {
            crate::render_prof::event("client_surface_patch.fallback.repaint");
            return Ok(false);
        }
        let rows = if self.draw_host_cursor {
            let Some(rows) = self
                .blit_encoder
                .patch_rows_with_drawn_cursor(&patch.rows, patch.cursor.as_ref())
            else {
                crate::render_prof::event("client_surface_patch.fallback.drawn_cursor");
                return Ok(false);
            };
            rows
        } else {
            patch.rows
        };
        let encode_started = crate::render_prof::timer();
        let Some(encoded) =
            self.blit_encoder
                .encode_patch(&rows, patch.cursor.clone(), self.draw_host_cursor)
        else {
            crate::render_prof::event("client_surface_patch.fallback.encode");
            return Ok(false);
        };
        crate::render_prof::duration_since("client_surface_patch.encode", encode_started);
        let write_started = crate::render_prof::timer();
        let mut stdout = io::stdout();
        stdout.write_all(&encoded.bytes)?;
        stdout.flush()?;
        crate::render_prof::duration_since("client_surface_patch.write", write_started);
        let committed = self.blit_encoder.commit_patch(&rows, patch.cursor, encoded);
        crate::render_prof::event(if committed {
            "client_surface_patch.success"
        } else {
            "client_surface_patch.fallback.commit"
        });
        Ok(committed)
    }

    fn present_frame(&mut self, frame_data: FrameData) {
        let frame_data = if self.draw_host_cursor {
            render_ansi::frame_with_drawn_cursor(frame_data)
        } else {
            frame_data
        };
        let encoded = if self.draw_host_cursor {
            self.blit_encoder
                .encode_with_suppressed_visible_cursor(&frame_data, self.repaint_pending)
        } else {
            self.blit_encoder.encode(&frame_data, self.repaint_pending)
        };
        let mut stdout = io::stdout();
        let graphics = if self.kitty_graphics_enabled {
            frame_data.graphics.as_slice()
        } else {
            &[]
        };
        let _ = write_encoded_frame_with_graphics(&mut stdout, &encoded.bytes, graphics);
        let _ = stdout.flush();
        self.blit_encoder.commit(frame_data, encoded);
        self.repaint_pending = false;
    }
}

// ---------------------------------------------------------------------------
// Client event loop
// ---------------------------------------------------------------------------

/// Internal events for the client event loop.
enum ClientLoopEvent {
    /// Raw input bytes from stdin.
    #[cfg(unix)]
    StdinInput(Vec<u8>),
    /// One confirmed SGR pixel report with geometry captured by the reader.
    #[cfg(unix)]
    PixelMouse(Vec<u8>, crate::input::mouse::HostGeometry),
    #[cfg(unix)]
    DirectGraphicsResponse(direct_graphics::Response),
    /// Structured input events from platforms without Unix-style stdin bytes.
    #[cfg(windows)]
    StdinEvents(Vec<crate::protocol::ClientInputEvent>),
    /// Terminal resize detected, including current exact-pixel eligibility.
    Resize(u16, u16, u32, u32, bool),
    /// Server message received.
    ServerMessage(Box<ServerMessage>),
    /// Server reader thread exited (connection lost).
    ServerDisconnected,
    /// Timer tick.
    Timer,
}

/// Runs the thin client: connects to the server, performs the handshake,
/// and enters the main event loop.
///
/// This is the entry point called from `main.rs` when running in client mode.
pub fn run_client() -> io::Result<()> {
    run_client_with_mode(None, None, "connecting to server")
}

/// Runs a direct terminal attach client.
#[cfg(unix)]
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        "attaching to terminal",
    )
}

/// Direct terminal attach is Unix raw-byte input only until Windows gets a semantic attach path.
#[cfg(windows)]
pub fn run_terminal_attach(_terminal_id: String, _takeover: bool) -> io::Result<()> {
    debug_assert!(!crate::platform::capabilities().direct_terminal_attach);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "direct terminal attach is not supported on Windows yet",
    ))
}

fn run_client_with_mode(
    attach_request: Option<(String, bool)>,
    attach_escape: Option<AttachEscapeState>,
    log_message: &'static str,
) -> io::Result<()> {
    init_logging();

    let loaded_config = crate::config::Config::load();
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    let client_rendered_shell = attach_request.is_none();
    let socket_path = client_socket_path();
    let keybinding_source = client_shell_keybinding_source();
    let startup_config_diagnostic =
        if keybinding_source == shell::ClientShellKeybindingSource::Endpoint {
            crate::config::config_diagnostic_summary_without_keybindings(&loaded_config.diagnostics)
        } else {
            crate::config::config_diagnostic_summary(&loaded_config.diagnostics)
        };
    let shell_config = client_rendered_shell.then(|| {
        shell::ClientShellConfig::from_config(&loaded_config.config)
            .with_startup_config_diagnostic(startup_config_diagnostic)
            .with_startup_onboarding(loaded_config.config.should_show_onboarding())
            .with_keybinding_source(keybinding_source)
            .with_local_endpoint(&socket_path)
    });
    let mouse_capture = loaded_config.config.ui.mouse_capture;
    let mouse_scroll_lines = loaded_config.config.ui.mouse_scroll_lines();
    let redraw_on_focus_gained = loaded_config.config.ui.redraw_on_focus_gained;
    let host_cursor = loaded_config.config.ui.host_cursor;
    let remote_image_paste_key = client_remote_image_paste_key(&loaded_config.config);
    let kitty_graphics_enabled =
        loaded_config.config.experimental.kitty_graphics && client_rendered_shell;
    let pixel_geometry_enabled = kitty_graphics_enabled || attach_escape.is_some();
    let loop_config = ClientLoopConfig {
        sound_config: loaded_config.config.ui.sound,
        mouse_scroll_lines,
        redraw_on_focus_gained,
        host_cursor,
        kitty_graphics_enabled,
        pixel_geometry_enabled,
        pixel_geometry_fallback: kitty_graphics_enabled,
        mouse_capture_active: mouse_capture,
        remote_image_paste_key,
        shell_config,
    };

    crate::logging::startup("client");
    info!(path = %socket_path.display(), "{log_message}");

    // Try to connect to the server.
    let mut stream = match crate::ipc::connect_local_stream(&socket_path) {
        Ok(s) => s,
        Err(err) => {
            // Server unreachable — show clear error and exit.
            let client_err = ClientError::ConnectionFailed(err);
            eprintln!("herdr: {client_err}");
            std::process::exit(1);
        }
    };

    // Get the terminal geometry before handshake (before raw mode).
    let (cols, rows, cell_width_px, cell_height_px, exact_cell_size) =
        initial_terminal_geometry(pixel_geometry_enabled, kitty_graphics_enabled);

    let shell_surface_size = loop_config
        .shell_config
        .as_ref()
        .map(|shell| shell.initial_surface_size(cols, rows));
    let endpoint_keybindings = loop_config
        .shell_config
        .as_ref()
        .is_some_and(shell::ClientShellConfig::uses_endpoint_keybindings);

    // Perform handshake while the stream is still in blocking mode.
    let handshake = match do_handshake(
        &mut stream,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        exact_cell_size,
        shell_surface_size,
        endpoint_keybindings,
        loop_config.mouse_capture_active,
    ) {
        Ok(encoding) => encoding,
        Err(err) => {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
    };

    if let Some((terminal_id, takeover)) = attach_request {
        let attach = ClientMessage::AttachTerminal {
            terminal_id,
            takeover,
        };
        if let Err(err) = write_to_server(&mut stream, &attach) {
            eprintln!("herdr: failed to request terminal attach: {err}");
            std::process::exit(1);
        }
    }

    // Now set up the terminal. This must happen AFTER the handshake succeeds,
    // so we don't leave the terminal in raw mode if the server rejects us.
    let direct_attach = attach_escape.is_some();
    let terminal_guard = if direct_attach {
        setup_direct_attach_terminal(mouse_capture)
    } else {
        setup_terminal(mouse_capture)
    }
    .map_err(|err| {
        eprintln!("herdr: failed to set up terminal: {err}");
        err
    })?;

    // Install a panic hook so the foreground client always restores its terminal.
    let panic_restore = terminal_guard.panic_restore();
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        panic_restore();
        original_hook(info);
    }));

    // Create the tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let should_quit = Arc::new(AtomicBool::new(false));

    // ctrlc's "termination" feature also catches SIGTERM/SIGHUP so direct
    // termination signals still run the quit path and TerminalGuard::Drop.
    let quit_flag = should_quit.clone();
    if let Err(err) = ctrlc::set_handler(move || {
        quit_flag.store(true, Ordering::Release);
    }) {
        warn!(%err, "failed to install termination handler; terminal restore relies on TerminalGuard::Drop and the panic hook");
    }

    let result = rt.block_on(async {
        run_client_loop(
            stream,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            exact_cell_size,
            should_quit,
            loop_config,
            handshake.encoding,
            handshake.endpoint_methods,
            attach_escape,
        )
        .await
    });

    // Restore the terminal before printing any final status message.
    let terminal_restore_failed = terminal_guard.restore().is_err();

    if let Err(err) = result {
        let _ = writeln!(io::stderr(), "herdr: {err}");
        rt.shutdown_timeout(Duration::from_millis(100));
        crate::logging::shutdown("client");

        let detached = matches!(
            &err,
            ClientError::ServerShutdown {
                reason: Some(reason)
            } if reason == "detached"
        );
        let connection_lost_during_terminal_hangup =
            terminal_restore_failed && matches!(&err, ClientError::ConnectionLost(_));
        if detached || connection_lost_during_terminal_hangup {
            return Ok(());
        }

        std::process::exit(1);
    }

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("client");
    Ok(())
}

fn dispatch_client_shell_actions(
    actions: Vec<shell::ClientShellAction>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    write_stream: &mut LocalStream,
    detached_process_children: &mut Vec<std::process::Child>,
) -> Result<Vec<crossterm::event::MouseEvent>, ClientError> {
    let mut replay_mouse = Vec::new();
    for action in actions {
        match action {
            shell::ClientShellAction::Endpoint { boot_id, request } => {
                endpoint_commands.enqueue(boot_id, request);
            }
            shell::ClientShellAction::ClipboardWrite(bytes) => {
                crate::selection::write_osc52_bytes(&bytes);
            }
            shell::ClientShellAction::Request(request) => {
                write_to_server(write_stream, &request).map_err(ClientError::ConnectionLost)?;
            }
            shell::ClientShellAction::OpenSafeWebUrl(url) => {
                if crate::app::actions::safe_web_url(&url).is_some() {
                    match crate::platform::open_url(&url) {
                        Ok(Some(child)) => detached_process_children.push(child),
                        Ok(None) => {}
                        Err(err) => warn!(err = %err, url = %url, "failed to open pane URL"),
                    }
                }
            }
            shell::ClientShellAction::ReplayMouse(events) => replay_mouse.extend(events),
            shell::ClientShellAction::Keybind(action) => {
                debug!(
                    ?action,
                    "client shell action awaits its presentation family"
                );
            }
        }
    }
    endpoint_commands
        .send_next(write_stream)
        .map_err(ClientError::ConnectionLost)?;
    Ok(replay_mouse)
}

fn client_shell_resize_message(
    shell: &shell::ClientShellState,
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    pixel_mouse: bool,
) -> ClientMessage {
    ClientMessage::ClientShellResize {
        cell_width_px,
        cell_height_px,
        surface_size: shell.surface_size(cols, rows),
        pixel_mouse,
    }
}

fn sync_client_shell_keyboard_report_all(state: &mut ClientState) -> Result<(), ClientError> {
    let Some(shell) = state.shell.as_ref() else {
        return Ok(());
    };
    let desired = state.pane_keyboard_report_all || shell.host_keyboard_report_all_requested();
    if desired == state.keyboard_report_all_active {
        return Ok(());
    }
    crate::terminal_modes::set_host_kitty_keyboard_report_all(&mut io::stdout(), desired)
        .map_err(ClientError::ConnectionFailed)?;
    state.keyboard_report_all_active = desired;
    Ok(())
}

fn apply_client_shell_input_source_changes(
    state: &mut ClientState,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
) {
    let changes = state
        .shell
        .as_mut()
        .map(shell::ClientShellState::take_input_source_changes)
        .unwrap_or_default();
    for active in changes {
        if active {
            prefix_input_source.switch_to_ascii();
        } else {
            prefix_input_source.restore();
        }
    }
}

fn install_client_shell_snapshot(
    state: &mut ClientState,
    snapshot: Box<crate::protocol::ClientShellSnapshot>,
    write_stream: &mut LocalStream,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
) -> Result<(), ClientError> {
    let (composed, resize, graphics_cleanup) = if let Some(shell) = &mut state.shell {
        let previous_size = shell.surface_size(state.reported_size.0, state.reported_size.1);
        shell.set_snapshot(snapshot);
        let graphics_cleanup = shell.take_pending_graphics_cleanup();
        let next_size = shell.surface_size(state.reported_size.0, state.reported_size.1);
        (
            shell.compose(state.reported_size.0, state.reported_size.1),
            (previous_size != next_size).then(|| {
                client_shell_resize_message(
                    shell,
                    state.reported_size.0,
                    state.reported_size.1,
                    state.reported_cell_size.0,
                    state.reported_cell_size.1,
                    state.pixel_geometry_exact,
                )
            }),
            graphics_cleanup,
        )
    } else {
        (None, None, Vec::new())
    };
    apply_client_shell_input_source_changes(state, prefix_input_source);
    state.present_graphics(&graphics_cleanup);
    if let Some(resize) = resize {
        write_to_server(write_stream, &resize).map_err(ClientError::ConnectionLost)?;
    }
    if let Some(frame) = composed {
        state.present_frame(frame);
    }
    Ok(())
}

fn finish_client_shell_input(
    state: &mut ClientState,
    outcome: shell::ClientShellInput,
    frame: Option<FrameData>,
    write_stream: &mut LocalStream,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
) -> Result<bool, ClientError> {
    apply_client_shell_input_source_changes(state, prefix_input_source);
    if outcome.detach {
        let _ = write_to_server(write_stream, &ClientMessage::Detach);
        return Ok(true);
    }
    if outcome.resize {
        let shell = state.shell.as_ref().expect("shell mode remains active");
        let resize = client_shell_resize_message(
            shell,
            state.reported_size.0,
            state.reported_size.1,
            state.reported_cell_size.0,
            state.reported_cell_size.1,
            state.pixel_geometry_exact,
        );
        write_to_server(write_stream, &resize).map_err(ClientError::ConnectionLost)?;
    }
    #[cfg(not(windows))]
    if outcome.query_host_appearance {
        query_host_terminal_appearance();
    }
    if outcome.query_host_theme {
        query_host_terminal_theme();
    }
    sync_client_shell_keyboard_report_all(state)?;
    let replay = dispatch_client_shell_actions(
        outcome.actions,
        endpoint_commands,
        write_stream,
        &mut state.detached_process_children,
    )?;
    debug_assert!(
        replay.is_empty(),
        "mouse replay only follows endpoint results"
    );
    for request in outcome.requests {
        write_to_server(write_stream, &request).map_err(ClientError::ConnectionLost)?;
    }
    if let Some(frame) = frame {
        state.present_frame(frame);
    }
    Ok(false)
}

/// The main client event loop.
///
/// Uses a threaded architecture:
/// - stdin reader thread → sends raw input bytes to main loop
/// - resize poller thread → sends resize events to main loop
/// - server reader thread → reads ServerMessages and sends to main loop
/// - main loop: coordinates input, output, and server communication
async fn run_client_loop(
    stream: LocalStream,
    cols: u16,
    rows: u16,
    initial_cell_width_px: u32,
    initial_cell_height_px: u32,
    initial_pixel_geometry_exact: bool,
    should_quit: Arc<AtomicBool>,
    config: ClientLoopConfig,
    negotiated_encoding: RenderEncoding,
    endpoint_methods: Option<Vec<String>>,
    attach_escape: Option<AttachEscapeState>,
) -> Result<(), ClientError> {
    #[cfg(windows)]
    let _ = config.mouse_scroll_lines;
    let draw_host_cursor = attach_escape.is_none() && should_draw_host_cursor(config.host_cursor);
    let is_remote_client = is_remote_client_process();

    let mut state = ClientState {
        blit_encoder: render_ansi::BlitEncoder::new(),
        mouse_capture_active: config.mouse_capture_active,
        endpoint_mouse_capture_requested: false,
        endpoint_sgr_pixels_requested: false,
        direct_mouse_capture_preference: attach_escape.is_some() && config.mouse_capture_active,
        shell_mouse_capture_preference: config.mouse_capture_active,
        direct_keyboard_protocol: crate::terminal_modes::DirectHostKeyboardState::default(),
        pane_keyboard_report_all: false,
        keyboard_report_all_active: false,
        reported_size: (cols, rows),
        reported_cell_size: (initial_cell_width_px, initial_cell_height_px),
        sound_config: config.sound_config,
        kitty_graphics_enabled: config.kitty_graphics_enabled,
        pixel_geometry_enabled: config.pixel_geometry_enabled,
        pixel_geometry_exact: initial_pixel_geometry_exact,
        #[cfg(unix)]
        direct_graphics_response: Arc::new(Mutex::new(direct_graphics::ResponseMatcher::default())),
        #[cfg(unix)]
        retired_direct_graphics: None,
        #[cfg(unix)]
        pending_surface_graphics: HashMap::new(),
        attach_escape,
        #[cfg(unix)]
        mouse_scroll_lines: config.mouse_scroll_lines,
        remote_image_paste_key: config.remote_image_paste_key,
        redraw_on_focus_gained: config.redraw_on_focus_gained,
        repaint_pending: false,
        draw_host_cursor,
        detached_process_children: Vec::new(),
        shell: config.shell_config.map(shell::ClientShellState::new),
    };
    if let Some(shell) = state.shell.as_mut() {
        shell.set_graphics_cell_size(initial_cell_width_px, initial_cell_height_px);
        shell.set_endpoint_methods(endpoint_methods);
    }
    debug!(?negotiated_encoding, "client render encoding active");
    let host_mouse_capture_active = Arc::new(AtomicBool::new(state.mouse_capture_active));
    // Cell size reported by the host terminal, packed as width<<32 | height.
    // Zero means the host has not reported one.
    let reported_cell_size = Arc::new(AtomicU64::new(0));
    let host_sgr_pixels_active = Arc::new(AtomicBool::new(false));

    // Channel for events from the resize and server reader threads.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);
    // Keep Windows console draining independent of server-frame backpressure.
    #[cfg(windows)]
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);
    #[cfg(unix)]
    let stdin_tx = event_tx.clone();

    let mut endpoint_commands = endpoint_commands::EndpointCommands::default();

    // Spawn the stdin reader thread.
    let will_query_host_terminal_theme =
        state.attach_escape.is_none() && should_query_host_terminal_theme();
    // Terminals behind ConPTY report no pixel size through the ioctl, so ask the
    // host terminal directly instead of falling back to an assumed cell size.
    let will_query_host_cell_size = state.attach_escape.is_none()
        && host_cell_size_query_required(state.kitty_graphics_enabled);
    let stdin_quit = should_quit.clone();
    let stdin_mouse_capture_active = host_mouse_capture_active.clone();
    let stdin_sgr_pixels_active = host_sgr_pixels_active.clone();
    #[cfg(unix)]
    let stdin_direct_response = state.direct_graphics_response.clone();
    #[cfg(unix)]
    let stdin_direct_response_active = stdin_direct_response
        .lock()
        .map(|matcher| matcher.active_handle())
        .unwrap_or_default();
    std::thread::spawn(move || {
        input::stdin_reader_loop(
            stdin_tx,
            &stdin_quit,
            will_query_host_terminal_theme,
            will_query_host_cell_size,
            stdin_mouse_capture_active,
            stdin_sgr_pixels_active,
            #[cfg(unix)]
            stdin_direct_response,
            #[cfg(unix)]
            stdin_direct_response_active,
        );
    });

    if will_query_host_terminal_theme {
        query_host_terminal_theme();
        #[cfg(not(windows))]
        if state.shell.is_some() {
            query_host_terminal_appearance();
        }
    }

    if will_query_host_cell_size {
        query_host_cell_size();
    }

    // Spawn the resize poller thread.
    let resize_quit = should_quit.clone();
    let resize_tx = event_tx.clone();
    let resize_cell_size = reported_cell_size.clone();
    let kitty_graphics_enabled = state.kitty_graphics_enabled;
    let pixel_geometry_enabled = state.pixel_geometry_enabled;
    let pixel_geometry_fallback = config.pixel_geometry_fallback;
    std::thread::spawn(move || {
        resize_poll_loop(
            resize_tx,
            cols,
            rows,
            initial_cell_width_px,
            initial_cell_height_px,
            initial_pixel_geometry_exact,
            pixel_geometry_enabled,
            pixel_geometry_fallback,
            &resize_cell_size,
            &resize_quit,
        );
    });

    // Spawn the server reader thread (blocking reads from the socket).
    // Clone the stream's file descriptor so we can read from a blocking stream.
    let server_read_quit = should_quit.clone();
    let server_read_tx = event_tx.clone();
    let read_stream = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    std::thread::spawn(move || {
        let max_frame_size = if kitty_graphics_enabled {
            MAX_GRAPHICS_FRAME_SIZE
        } else {
            MAX_FRAME_SIZE
        };
        server_reader_thread(
            read_stream,
            server_read_tx,
            &server_read_quit,
            max_frame_size,
        );
    });

    // Use the original stream for writing (blocking is fine since we write
    // from the async loop).
    let mut write_stream = stream;
    write_stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    // This (foreground) client owns the prefix ASCII input-source switch
    // (implemented on macOS and Windows; a no-op on other platforms).
    let mut prefix_input_source = crate::platform::RealPrefixInputSource::default();

    // Main event loop.
    let mut client_timer = timer::ClientLoopTimer::new();
    #[cfg(windows)]
    let mut stdin_open = true;
    while !should_quit.load(Ordering::Acquire) {
        let timer_delay = state
            .shell
            .as_ref()
            .map_or(Duration::from_millis(100), |shell| {
                shell.timer_delay(std::time::Instant::now())
            });
        let timer_deadline = client_timer.deadline(std::time::Instant::now(), timer_delay);
        #[cfg(windows)]
        let event = tokio::select! {
            _ = tokio::time::sleep_until(timer_deadline.into()) => ClientLoopEvent::Timer,
            ev = stdin_rx.recv(), if stdin_open => match ev {
                Some(event) => event,
                None => {
                    stdin_open = false;
                    ClientLoopEvent::Timer
                }
            },
            ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
        };
        #[cfg(unix)]
        let event = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(timer_deadline.into()) => ClientLoopEvent::Timer,
            ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
        };
        let now = std::time::Instant::now();
        if let Some(shell) = state.shell.as_mut() {
            shell.tick_popup_pending(now);
        }

        match event {
            #[cfg(unix)]
            ClientLoopEvent::StdinInput(data) => {
                if state.shell.is_some() {
                    if will_query_host_cell_size {
                        let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
                        if let Some((width_px, height_px)) = reported_cell_size_from_events(&events)
                        {
                            store_reported_cell_size(&reported_cell_size, width_px, height_px);
                        }
                    }
                    let image_target = state
                        .shell
                        .as_ref()
                        .and_then(|shell| shell.clipboard_image_target());
                    if let Some(target) = image_target.clone() {
                        if should_bridge_clipboard_image_paste(
                            &data,
                            is_remote_client,
                            state.remote_image_paste_key,
                        ) {
                            if let Some(image) = crate::platform::read_clipboard_image() {
                                write_remote_image_to_server(
                                    &mut write_stream,
                                    target,
                                    image,
                                    "clipboard paste",
                                )?;
                                continue;
                            }
                            info!(
                                "clipboard image paste trigger received, but local clipboard has no image"
                            );
                        }
                        if let Some(image) =
                            read_image_file_from_terminal_drop(&data, is_remote_client)
                        {
                            write_remote_image_to_server(
                                &mut write_stream,
                                target,
                                image,
                                "file drop",
                            )?;
                            continue;
                        }
                    }
                    let (outcome, frame) = {
                        let shell = state.shell.as_mut().expect("checked shell mode");
                        let outcome = shell.handle_input_bytes(&data);
                        let frame = outcome
                            .repaint
                            .then(|| shell.compose(state.reported_size.0, state.reported_size.1))
                            .flatten();
                        (outcome, frame)
                    };
                    if finish_client_shell_input(
                        &mut state,
                        outcome,
                        frame,
                        &mut write_stream,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                    )? {
                        return Ok(());
                    }
                    continue;
                }
                let data = if let Some(attach_escape) = &mut state.attach_escape {
                    match attach_escape.filter_input(
                        data,
                        state.reported_size.1,
                        state.mouse_scroll_lines,
                    ) {
                        AttachInputAction::Forward(data) => data,
                        AttachInputAction::ForwardPair(first, second) => {
                            for data in [first, second] {
                                if let Err(e) = write_to_server(
                                    &mut write_stream,
                                    &ClientMessage::Input { data },
                                ) {
                                    return Err(ClientError::ConnectionLost(e));
                                }
                            }
                            continue;
                        }
                        AttachInputAction::Semantic(action) => {
                            if let Err(e) = write_attach_semantic_action(&mut write_stream, action)
                            {
                                return Err(ClientError::ConnectionLost(e));
                            }
                            continue;
                        }
                        AttachInputAction::ForwardThenSemantic(prefix, action) => {
                            if let Err(e) = write_to_server(
                                &mut write_stream,
                                &ClientMessage::Input { data: prefix },
                            ) {
                                return Err(ClientError::ConnectionLost(e));
                            }
                            if let Err(e) = write_attach_semantic_action(&mut write_stream, action)
                            {
                                return Err(ClientError::ConnectionLost(e));
                            }
                            continue;
                        }
                        AttachInputAction::Detach => {
                            let _ = write_to_server(&mut write_stream, &ClientMessage::Detach);
                            return Ok(());
                        }
                        AttachInputAction::None => continue,
                    }
                } else {
                    let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
                    if crate::raw_input::events_require_host_surface_redraw(
                        &events,
                        state.redraw_on_focus_gained,
                    ) {
                        state.request_repaint();
                    }
                    if crate::raw_input::events_require_host_terminal_appearance_query(&events) {
                        query_host_terminal_appearance();
                    }
                    if crate::raw_input::events_require_host_terminal_theme_query(&events) {
                        query_host_terminal_theme();
                    }
                    if let Some((width_px, height_px)) = reported_cell_size_from_events(&events) {
                        store_reported_cell_size(&reported_cell_size, width_px, height_px);
                    }
                    data
                };
                if should_bridge_clipboard_image_paste(
                    &data,
                    is_remote_client,
                    state.remote_image_paste_key,
                ) {
                    if let Some(image) = crate::platform::read_clipboard_image() {
                        write_remote_image_to_server(
                            &mut write_stream,
                            crate::protocol::ClientClipboardImageTarget::DirectTerminal,
                            image,
                            "clipboard paste",
                        )?;
                        continue;
                    }
                    info!(
                        "clipboard image paste trigger received, but local clipboard has no image"
                    );
                }
                if let Some(image) = read_image_file_from_terminal_drop(&data, is_remote_client) {
                    write_remote_image_to_server(
                        &mut write_stream,
                        crate::protocol::ClientClipboardImageTarget::DirectTerminal,
                        image,
                        "file drop",
                    )?;
                    continue;
                }
                let msg = ClientMessage::Input { data };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            #[cfg(unix)]
            ClientLoopEvent::DirectGraphicsResponse(response) => {
                let composed = state
                    .pending_surface_graphics
                    .remove(&response.transfer_id)
                    .filter(|_| response.success)
                    .and_then(|asset| {
                        let shell = state.shell.as_mut()?;
                        shell
                            .trust_direct_graphics_asset(&asset, response.image_id)
                            .then(|| shell.compose(state.reported_size.0, state.reported_size.1))
                            .flatten()
                    });
                let message = ClientMessage::GraphicsTransmissionResult {
                    transfer_id: response.transfer_id,
                    image_id: response.image_id,
                    success: response.success,
                };
                if let Err(err) = write_to_server(&mut write_stream, &message) {
                    return Err(ClientError::ConnectionLost(err));
                }
                if let Some(frame) = composed {
                    state.present_frame(frame);
                }
            }
            #[cfg(unix)]
            ClientLoopEvent::PixelMouse(data, geometry) => {
                if state.shell.is_some() {
                    let (outcome, frame) = {
                        let shell = state.shell.as_mut().expect("checked shell mode");
                        let outcome = shell.handle_pixel_mouse(&data, geometry);
                        let frame = outcome
                            .repaint
                            .then(|| shell.compose(state.reported_size.0, state.reported_size.1))
                            .flatten();
                        (outcome, frame)
                    };
                    if finish_client_shell_input(
                        &mut state,
                        outcome,
                        frame,
                        &mut write_stream,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                    )? {
                        return Ok(());
                    }
                    continue;
                }
                if let Some(attach_escape) = state.attach_escape.as_mut() {
                    if let Some(prefix) = attach_escape.take_pending_prefix() {
                        if let Err(err) = write_to_server(
                            &mut write_stream,
                            &ClientMessage::Input { data: prefix },
                        ) {
                            return Err(ClientError::ConnectionLost(err));
                        }
                    }
                    if let Some((kind, position, modifiers)) =
                        direct_attach_pixel_mouse(&data, geometry)
                    {
                        let message = ClientMessage::AttachMouse {
                            kind,
                            position,
                            geometry: Some(crate::protocol::ClientMouseGeometry {
                                cols: geometry.cols,
                                rows: geometry.rows,
                                width_px: geometry.width_px,
                                height_px: geometry.height_px,
                            }),
                            modifiers,
                            lines: state.mouse_scroll_lines.max(1).min(u16::MAX as usize) as u16,
                        };
                        if let Err(err) = write_to_server(&mut write_stream, &message) {
                            return Err(ClientError::ConnectionLost(err));
                        }
                    }
                }
            }
            #[cfg(windows)]
            ClientLoopEvent::StdinEvents(events) => {
                if state.shell.is_some() {
                    let image_target = state
                        .shell
                        .as_ref()
                        .and_then(|shell| shell.clipboard_image_target());
                    if let Some(target) = image_target.clone() {
                        if should_bridge_clipboard_image_events(
                            &events,
                            is_remote_client,
                            state.remote_image_paste_key,
                        ) {
                            if let Some(image) = crate::platform::read_clipboard_image() {
                                write_remote_image_to_server(
                                    &mut write_stream,
                                    target,
                                    image,
                                    "clipboard paste",
                                )?;
                                continue;
                            }
                            info!(
                                "clipboard image paste trigger received, but local clipboard has no image"
                            );
                        }
                        if let Some(image) =
                            read_image_file_from_client_events(&events, is_remote_client)
                        {
                            write_remote_image_to_server(
                                &mut write_stream,
                                target,
                                image,
                                "file drop",
                            )?;
                            continue;
                        }
                    }
                    let (outcome, frame) = {
                        let shell = state.shell.as_mut().expect("checked shell mode");
                        let outcome = shell.handle_client_events(&events);
                        let frame = outcome
                            .repaint
                            .then(|| shell.compose(state.reported_size.0, state.reported_size.1))
                            .flatten();
                        (outcome, frame)
                    };
                    if finish_client_shell_input(
                        &mut state,
                        outcome,
                        frame,
                        &mut write_stream,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                    )? {
                        return Ok(());
                    }
                    continue;
                }
                // Direct terminal attach is Unix-only; every Windows client uses ClientShell.
            }
            ClientLoopEvent::Resize(
                new_cols,
                new_rows,
                cell_width_px,
                cell_height_px,
                pixel_geometry_exact,
            ) => {
                if !pixel_geometry_exact && host_sgr_pixels_active.load(Ordering::Acquire) {
                    set_mouse_capture(state.mouse_capture_active, false)
                        .map_err(ClientError::ConnectionFailed)?;
                    host_sgr_pixels_active.store(false, Ordering::Release);
                }
                state.reported_size = (new_cols, new_rows);
                state.reported_cell_size = (cell_width_px, cell_height_px);
                state.pixel_geometry_exact = pixel_geometry_exact;
                // Resizing invalidates both the host-side blit baseline and pane hit geometry.
                state.request_repaint();
                if let Some(shell) = state.shell.as_mut() {
                    shell.set_graphics_cell_size(cell_width_px, cell_height_px);
                    shell.invalidate_pane_surface();
                }
                let msg = if let Some(shell) = &state.shell {
                    client_shell_resize_message(
                        shell,
                        new_cols,
                        new_rows,
                        cell_width_px,
                        cell_height_px,
                        pixel_geometry_exact,
                    )
                } else {
                    ClientMessage::Resize {
                        cols: new_cols,
                        rows: new_rows,
                        cell_width_px,
                        cell_height_px,
                        pixel_mouse: pixel_geometry_exact,
                    }
                };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::ServerMessage(msg) => match *msg {
                ServerMessage::ClientShellSnapshot(_) => {
                    return Err(ClientError::Protocol(protocol::FramingError::Io(
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "server sent an unnegotiated binary endpoint snapshot",
                        ),
                    )));
                }
                ServerMessage::PaneSurface(surface) => {
                    let composed = if let Some(shell) = &mut state.shell {
                        shell.set_pane_surface(surface);
                        shell.compose(state.reported_size.0, state.reported_size.1)
                    } else {
                        None
                    };
                    apply_client_shell_input_source_changes(&mut state, &mut prefix_input_source);
                    if let Some(frame) = composed {
                        state.present_frame(frame);
                    }
                }
                ServerMessage::PaneSurfacePatch(patch) => {
                    let patch_started = crate::render_prof::timer();
                    let apply_started = crate::render_prof::timer();
                    let outcome = state
                        .shell
                        .as_mut()
                        .map(|shell| shell.apply_pane_surface_patch(patch));
                    crate::render_prof::duration_since("client_surface_patch.apply", apply_started);
                    let compose_fallback = match outcome {
                        Some(shell::ClientPaneSurfacePatchOutcome::Applied(Some(patch))) => {
                            match state.present_surface_patch(patch) {
                                Ok(presented) => !presented,
                                Err(error) => {
                                    warn!(%error, "failed to present retained pane surface patch");
                                    state.request_repaint();
                                    false
                                }
                            }
                        }
                        Some(shell::ClientPaneSurfacePatchOutcome::Applied(None)) => true,
                        Some(shell::ClientPaneSurfacePatchOutcome::Rejected) | None => false,
                    };
                    apply_client_shell_input_source_changes(&mut state, &mut prefix_input_source);
                    if compose_fallback {
                        let composed = state.shell.as_mut().and_then(|shell| {
                            shell.compose(state.reported_size.0, state.reported_size.1)
                        });
                        if let Some(frame) = composed {
                            state.present_frame(frame);
                        }
                    }
                    crate::render_prof::duration_since("client_surface_patch.total", patch_started);
                    crate::render_prof::flush_if_due();
                }
                ServerMessage::Terminal(frame) => {
                    if state.kitty_graphics_enabled && contains_kitty_graphics_bytes(&frame.bytes) {
                        record_received_kitty_graphics(&frame.bytes);
                    }
                    let mut stdout = io::stdout();
                    let _ = stdout.write_all(&frame.bytes);
                    let _ = stdout.flush();
                }
                ServerMessage::Graphics { bytes } => {
                    if state.kitty_graphics_enabled {
                        record_received_kitty_graphics(&bytes);
                        let mut stdout = io::stdout();
                        let _ = stdout.write_all(&bytes);
                        let _ = stdout.flush();
                    }
                }
                ServerMessage::TerminalBell { count } => {
                    if let Err(err) =
                        crate::terminal_effects::write_terminal_bells(&mut io::stdout(), count)
                    {
                        warn!(err = %err, "failed to emit terminal bell");
                    }
                }
                ServerMessage::GraphicsFile {
                    path,
                    expected_len,
                    image_id,
                    transfer_id,
                    leading,
                    control,
                    surface_asset,
                } => {
                    #[cfg(unix)]
                    {
                        if state.retired_direct_graphics.take() == Some((transfer_id, image_id)) {
                            continue;
                        }
                        let surface_asset_valid = match (state.shell.as_ref(), &surface_asset) {
                            (Some(shell), Some(asset)) => {
                                crate::kitty_graphics::surface::host_image_id(
                                    shell.graphics_scope(),
                                    asset,
                                ) == image_id
                            }
                            (None, None) => true,
                            _ => false,
                        };
                        let valid = state.kitty_graphics_enabled
                            && surface_asset_valid
                            && usize::try_from(expected_len).ok().is_some_and(|len| {
                                crate::pane_graphics_files::validate_direct_source(
                                    std::path::Path::new(&path),
                                    len,
                                )
                                .is_ok()
                                    && direct_graphics::valid_control(&control, image_id, len)
                            })
                            && state
                                .direct_graphics_response
                                .lock()
                                .is_ok_and(|mut matcher| matcher.arm(transfer_id, image_id));
                        let sent = if valid {
                            let mut command = Vec::new();
                            crate::kitty_graphics::encode_kitty_regular_file(
                                &mut command,
                                &leading,
                                &control,
                                &path,
                            );
                            let mut stdout = io::stdout();
                            let written = stdout
                                .write_all(&command)
                                .and_then(|()| stdout.flush())
                                .is_ok();
                            if written {
                                record_received_kitty_graphics(&command);
                            }
                            written
                        } else {
                            false
                        };
                        if sent {
                            if let Some(asset) = surface_asset {
                                state.pending_surface_graphics.insert(transfer_id, asset);
                            }
                            if let Ok(mut matcher) = state.direct_graphics_response.lock() {
                                matcher.start(transfer_id);
                            }
                            let started = ClientMessage::GraphicsTransmissionStarted {
                                transfer_id,
                                image_id,
                            };
                            if let Err(err) = write_to_server(&mut write_stream, &started) {
                                return Err(ClientError::ConnectionLost(err));
                            }
                        } else {
                            state.pending_surface_graphics.remove(&transfer_id);
                            if let Ok(mut matcher) = state.direct_graphics_response.lock() {
                                if valid {
                                    matcher.retire(transfer_id);
                                } else {
                                    matcher.cancel(transfer_id);
                                }
                            }
                            let result = ClientMessage::GraphicsTransmissionResult {
                                transfer_id,
                                image_id,
                                success: false,
                            };
                            if let Err(err) = write_to_server(&mut write_stream, &result) {
                                return Err(ClientError::ConnectionLost(err));
                            }
                        }
                    }
                    #[cfg(not(unix))]
                    let _ = (
                        path,
                        expected_len,
                        image_id,
                        transfer_id,
                        leading,
                        control,
                        surface_asset,
                    );
                }
                ServerMessage::GraphicsTransmissionRetired {
                    transfer_id,
                    image_id,
                } => {
                    #[cfg(unix)]
                    {
                        state.retired_direct_graphics = Some((transfer_id, image_id));
                        state.pending_surface_graphics.remove(&transfer_id);
                        let cleanup = state.shell.as_mut().map_or_else(Vec::new, |shell| {
                            shell.retire_direct_graphics_image(image_id);
                            shell
                                .compose(state.reported_size.0, state.reported_size.1)
                                .map(|frame| frame.graphics)
                                .unwrap_or_else(|| shell.take_pending_graphics_cleanup())
                        });
                        state.present_graphics(&cleanup);
                        if let Ok(mut matcher) = state.direct_graphics_response.lock() {
                            matcher.retire(transfer_id);
                        }
                    }
                    #[cfg(not(unix))]
                    let _ = (transfer_id, image_id);
                }
                ServerMessage::ServerShutdown { reason } => {
                    return Err(ClientError::ServerShutdown { reason });
                }
                ServerMessage::Notify {
                    kind,
                    message,
                    body,
                } => {
                    if state.shell.is_none() {
                        handle_notify(kind, &message, body.as_deref(), &state.sound_config);
                    }
                }
                ServerMessage::SemanticNotification(event) => {
                    if state.shell.is_some() {
                        let (effects, frame) = {
                            let shell = state.shell.as_mut().expect("checked shell mode");
                            let (effects, repaint) =
                                shell.receive_notification(event, std::time::Instant::now());
                            let frame = repaint
                                .then(|| {
                                    shell.compose(state.reported_size.0, state.reported_size.1)
                                })
                                .flatten();
                            (effects, frame)
                        };
                        handle_shell_notification_effects(effects, &state.sound_config);
                        if let Some(frame) = frame {
                            state.present_frame(frame);
                        }
                    }
                }
                ServerMessage::ClientShellError { message } => {
                    if let Some(shell) = state.shell.as_mut() {
                        if shell.receive_endpoint_error(message) {
                            let frame = shell.compose(state.reported_size.0, state.reported_size.1);
                            if let Some(frame) = frame {
                                state.present_frame(frame);
                            }
                        }
                    }
                }
                ServerMessage::ClientShellEndpointResponseChunk {
                    boot_id,
                    request_id,
                    final_chunk,
                    data,
                } => {
                    let completed = endpoint_commands
                        .receive_chunk(&boot_id, &request_id, final_chunk, data)
                        .map_err(ClientError::ConnectionLost)?;
                    let Some(completed) = completed else {
                        continue;
                    };
                    let (repaint, actions) = state.shell.as_mut().map_or_else(
                        || (false, Vec::new()),
                        |shell| {
                            shell.handle_endpoint_result(
                                &completed.boot_id,
                                &completed.request_id,
                                completed.result,
                            )
                        },
                    );
                    if let Some(shell) = state.shell.as_mut() {
                        shell.reconcile_input_source();
                    }
                    apply_client_shell_input_source_changes(&mut state, &mut prefix_input_source);
                    let replay_mouse = dispatch_client_shell_actions(
                        actions,
                        &mut endpoint_commands,
                        &mut write_stream,
                        &mut state.detached_process_children,
                    )?;
                    if replay_mouse.is_empty() {
                        if repaint {
                            if let Some(frame) = state.shell.as_mut().and_then(|shell| {
                                shell.compose(state.reported_size.0, state.reported_size.1)
                            }) {
                                state.present_frame(frame);
                            }
                        }
                    } else {
                        let (outcome, frame) = {
                            let shell = state.shell.as_mut().expect("shell endpoint response");
                            let mut outcome = shell.replay_mouse_events(replay_mouse);
                            outcome.repaint |= repaint;
                            let frame = outcome
                                .repaint
                                .then(|| {
                                    shell.compose(state.reported_size.0, state.reported_size.1)
                                })
                                .flatten();
                            (outcome, frame)
                        };
                        if finish_client_shell_input(
                            &mut state,
                            outcome,
                            frame,
                            &mut write_stream,
                            &mut endpoint_commands,
                            &mut prefix_input_source,
                        )? {
                            return Ok(());
                        }
                    }
                }
                ServerMessage::Clipboard { data } => {
                    if forward_clipboard(&data) {
                        let (width, height) = state.reported_size;
                        let frame = state.shell.as_mut().and_then(|shell| {
                            shell
                                .show_copy_feedback(std::time::Instant::now())
                                .then(|| shell.compose(width, height))
                                .flatten()
                        });
                        if let Some(frame) = frame {
                            state.present_frame(frame);
                        }
                    }
                    let _ = io::stdout().flush();
                }
                ServerMessage::WindowTitle { title } => {
                    let _ = crate::terminal_effects::write_window_title(
                        &mut io::stdout(),
                        title.as_deref(),
                    );
                }
                ServerMessage::ReloadSoundConfig => {
                    let previous_mouse_capture = state.shell_mouse_capture_preference;
                    let mut mouse_capture = previous_mouse_capture;
                    reload_local_client_config(
                        &mut state.sound_config,
                        &mut state.redraw_on_focus_gained,
                        &mut state.draw_host_cursor,
                        &mut state.remote_image_paste_key,
                        &mut mouse_capture,
                    );
                    state.shell_mouse_capture_preference = mouse_capture;
                    state.direct_mouse_capture_preference =
                        state.attach_escape.is_some() && mouse_capture;
                    if state.shell.is_some() && previous_mouse_capture != mouse_capture {
                        write_to_server(
                            &mut write_stream,
                            &ClientMessage::ClientShellMouseCapture {
                                enabled: mouse_capture,
                            },
                        )
                        .map_err(ClientError::ConnectionLost)?;
                    }
                    if state.attach_escape.is_some() {
                        let enabled = effective_mouse_capture(
                            state.endpoint_mouse_capture_requested,
                            state.direct_mouse_capture_preference,
                        );
                        let sgr_pixels = effective_sgr_pixel_mouse(
                            enabled,
                            state.endpoint_sgr_pixels_requested,
                            state.pixel_geometry_exact,
                        );
                        if enabled != state.mouse_capture_active
                            || sgr_pixels != host_sgr_pixels_active.load(Ordering::Acquire)
                        {
                            set_mouse_capture(enabled, sgr_pixels)
                                .map_err(ClientError::ConnectionFailed)?;
                        }
                        state.mouse_capture_active = enabled;
                        host_mouse_capture_active.store(enabled, Ordering::Release);
                        host_sgr_pixels_active.store(sgr_pixels, Ordering::Release);
                    }
                    let (frame, resize) = if let Some(shell) = state.shell.as_mut() {
                        let previous_size =
                            shell.surface_size(state.reported_size.0, state.reported_size.1);
                        shell.reload_client_config();
                        let next_size =
                            shell.surface_size(state.reported_size.0, state.reported_size.1);
                        let resize = (previous_size != next_size).then(|| {
                            shell.invalidate_pane_surface();
                            client_shell_resize_message(
                                shell,
                                state.reported_size.0,
                                state.reported_size.1,
                                state.reported_cell_size.0,
                                state.reported_cell_size.1,
                                state.pixel_geometry_exact,
                            )
                        });
                        (
                            shell.compose(state.reported_size.0, state.reported_size.1),
                            resize,
                        )
                    } else {
                        (None, None)
                    };
                    apply_client_shell_input_source_changes(&mut state, &mut prefix_input_source);
                    if let Some(resize) = resize {
                        write_to_server(&mut write_stream, &resize)
                            .map_err(ClientError::ConnectionLost)?;
                    }
                    if let Some(frame) = frame {
                        state.present_frame(frame);
                    }
                }
                ServerMessage::MouseCapture {
                    enabled,
                    sgr_pixels,
                } => {
                    state.endpoint_mouse_capture_requested = enabled;
                    state.endpoint_sgr_pixels_requested = sgr_pixels;
                    let enabled =
                        effective_mouse_capture(enabled, state.direct_mouse_capture_preference);
                    let next_sgr_pixels =
                        effective_sgr_pixel_mouse(enabled, sgr_pixels, state.pixel_geometry_exact);
                    let mouse_mode_changed = enabled != state.mouse_capture_active
                        || next_sgr_pixels != host_sgr_pixels_active.load(Ordering::Acquire);
                    if mouse_mode_changed {
                        #[cfg(windows)]
                        if enabled && windows_vti_input_backend_enabled() && is_ssh_session() {
                            let _ = enable_windows_virtual_terminal_input();
                        }
                        set_mouse_capture(enabled, next_sgr_pixels)
                            .map_err(ClientError::ConnectionFailed)?;
                        #[cfg(windows)]
                        if enabled && windows_vti_input_backend_enabled() && !is_ssh_session() {
                            let _ = enable_windows_virtual_terminal_input();
                        }
                    }
                    state.mouse_capture_active = enabled;
                    host_mouse_capture_active.store(enabled, Ordering::Release);
                    host_sgr_pixels_active.store(next_sgr_pixels, Ordering::Release);
                }
                ServerMessage::DirectTerminalKeyboardProtocol {
                    flags,
                    modify_other_keys_level,
                } => {
                    if state.attach_escape.is_some() {
                        crate::terminal_modes::set_direct_host_keyboard_protocol(
                            &mut io::stdout(),
                            &mut state.direct_keyboard_protocol,
                            flags,
                            modify_other_keys_level,
                        )
                        .map_err(ClientError::ConnectionFailed)?;
                    }
                }
                ServerMessage::ClientShellKeyboardReportAll { enabled } => {
                    if state.shell.is_some() {
                        state.pane_keyboard_report_all = enabled;
                        sync_client_shell_keyboard_report_all(&mut state)?;
                    }
                }
                ServerMessage::EndpointControl { kind, data } => {
                    if kind != crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND {
                        if kind.starts_with("shell.snapshot.") {
                            return Err(ClientError::Protocol(protocol::FramingError::Io(
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!(
                                        "unsupported mandatory endpoint snapshot codec {kind:?}"
                                    ),
                                ),
                            )));
                        }
                        debug!(%kind, "ignoring unknown endpoint control message");
                        continue;
                    }
                    let snapshot: crate::protocol::ClientShellSnapshot =
                        serde_json::from_str(&data).map_err(|error| {
                            ClientError::Protocol(protocol::FramingError::Io(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("invalid endpoint snapshot: {error}"),
                            )))
                        })?;
                    install_client_shell_snapshot(
                        &mut state,
                        Box::new(snapshot),
                        &mut write_stream,
                        &mut prefix_input_source,
                    )?;
                }
                ServerMessage::Welcome { .. } => {
                    debug!("received unexpected Welcome in main loop");
                }
            },
            ClientLoopEvent::ServerDisconnected => {
                return Err(ClientError::ConnectionLost(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed connection",
                )));
            }
            ClientLoopEvent::Timer => {
                client_timer.fired();
                #[cfg(unix)]
                if let Ok(mut matcher) = state.direct_graphics_response.lock() {
                    matcher.expire();
                }
                state
                    .detached_process_children
                    .retain_mut(|child| child.try_wait().ok().flatten().is_none());
                if state.shell.is_some() {
                    let expired_endpoint = endpoint_commands.expire(now);
                    let (effects, outcome, frame) = {
                        let shell = state.shell.as_mut().expect("checked shell mode");
                        let mut outcome = shell.tick_selection_autoscroll(now);
                        if let Some(expired) = expired_endpoint {
                            let (repaint, actions) = shell.handle_endpoint_result(
                                &expired.boot_id,
                                &expired.request_id,
                                expired.result,
                            );
                            outcome.repaint |= repaint;
                            outcome.actions.extend(actions);
                        }
                        let (effects, notification_repaint) = shell.tick_notifications(now);
                        outcome.repaint |= notification_repaint | shell.tick_copy_feedback(now);
                        let frame = outcome
                            .repaint
                            .then(|| shell.compose(state.reported_size.0, state.reported_size.1))
                            .flatten();
                        (effects, outcome, frame)
                    };
                    handle_shell_notification_effects(effects, &state.sound_config);
                    if finish_client_shell_input(
                        &mut state,
                        outcome,
                        frame,
                        &mut write_stream,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                    )? {
                        return Ok(());
                    }
                }
            }
        }
    }

    // Clean exit (Ctrl+C). Send Detach before closing.
    let detach = ClientMessage::Detach;
    let _ = write_to_server(&mut write_stream, &detach);
    let _ = io::stdout().flush();

    Ok(())
}

// ---------------------------------------------------------------------------
// Server reader thread
// ---------------------------------------------------------------------------

/// Blocking thread that reads ServerMessages from the server and sends them
/// to the main event loop.
fn server_reader_thread(
    mut stream: LocalStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
) {
    // Ensure the read stream is in blocking mode to avoid WouldBlock errors
    // from read_exact inside read_message. The stream should already be
    // blocking after handshake, but we enforce it here as a safety measure.
    if stream.set_nonblocking(false).is_err() {
        // If we can't set blocking mode, the stream is likely broken.
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
        return;
    }

    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        match protocol::read_message(&mut stream, max_frame_size) {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage(Box::new(msg)))
                    .is_err()
                {
                    break; // Main loop gone.
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                // Server closed connection.
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                // Should not happen with blocking mode, but handle gracefully
                // in case the stream was set nonblocking by another clone.
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Write helper
// ---------------------------------------------------------------------------

/// Writes a message to the server stream (blocking).
fn write_to_server(stream: &mut LocalStream, msg: &ClientMessage) -> io::Result<()> {
    protocol::write_message(stream, msg).map_err(|e| io::Error::other(e.to_string()))
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

fn reload_local_client_config(
    sound_config: &mut crate::config::SoundConfig,
    redraw_on_focus_gained: &mut bool,
    draw_host_cursor: &mut bool,
    remote_image_paste_key: &mut Option<(
        crossterm::event::KeyCode,
        crossterm::event::KeyModifiers,
    )>,
    mouse_capture: &mut bool,
) {
    match crate::config::load_live_config() {
        Ok(loaded) => {
            let invalid_section = |section: &str| {
                loaded
                    .invalid_sections
                    .iter()
                    .any(|invalid| invalid == section)
            };
            if !invalid_section("ui") && loaded.config.invalid_sidebar_bounds_diagnostic().is_none()
            {
                for diagnostic in loaded.config.ui.sound.diagnostics() {
                    warn!(diagnostic = %diagnostic, "local sound config diagnostic");
                }
                *sound_config = loaded.config.ui.sound.clone();
                *redraw_on_focus_gained = loaded.config.ui.redraw_on_focus_gained;
                *draw_host_cursor = should_draw_host_cursor(loaded.config.ui.host_cursor);
                *mouse_capture = loaded.config.ui.mouse_capture;
            }
            if !invalid_section("keys") {
                *remote_image_paste_key = client_remote_image_paste_key(&loaded.config);
            }
            debug!("reloaded local client config");
        }
        Err(diagnostics) => {
            warn!(diagnostics = ?diagnostics, "failed to reload local client config; keeping current client config");
        }
    }
}

// ---------------------------------------------------------------------------
// Clipboard forwarding
// ---------------------------------------------------------------------------

/// Decode a clipboard payload forwarded by the server.
fn decode_clipboard_payload(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Forwards a clipboard write from the server to the local client clipboard.
fn forward_clipboard(data: &str) -> bool {
    let Some(bytes) = decode_clipboard_payload(data) else {
        warn!("received invalid clipboard payload from server");
        return false;
    };

    crate::selection::write_osc52_bytes(&bytes);
    true
}

fn init_logging() {
    crate::logging::init_file_logging("herdr-client.log");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
