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
mod catalog_reload;
mod clipboard_forwarding;
mod clipboard_images;
mod config_reload;
#[cfg(unix)]
mod direct_graphics;
pub(crate) mod endpoint;
mod endpoint_commands;
mod errors;
mod events;
mod frame_output;
mod handshake;
mod input;
mod loop_config;
mod notifications;
mod shell;
mod shell_runtime;
mod startup;
mod state;
mod terminal_geometry;
mod terminal_sessions;
mod terminal_setup;
mod timer;
mod transport;

#[cfg(test)]
use clipboard_forwarding::decode_clipboard_payload;
use clipboard_forwarding::forward_clipboard;
#[cfg(test)]
use config_reload::reload_local_client_config;
use config_reload::{apply_reload, init_logging};
use events::ClientLoopEvent;
use loop_config::ClientLoopConfig;
use shell_runtime::*;
use state::ClientState;
use transport::*;

#[cfg(test)]
pub(crate) use shell::{ClientShellConfig, ClientShellState};
pub use startup::{run_client, run_terminal_attach};
pub use terminal_sessions::{run_terminal_session_control, run_terminal_session_observe};

#[cfg(not(windows))]
use terminal_geometry::query_host_terminal_appearance;
#[cfg(test)]
use terminal_geometry::{
    cell_size_fallback, current_terminal_geometry_with, ioctl_cell_size, pack_cell_size,
    resize_report_required, should_query_host_cell_size, write_host_cell_size_query,
    write_host_terminal_appearance_query, write_host_terminal_theme_query,
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
use clipboard_images::{
    client_remote_image_paste_key, endpoint_accepts_local_images, write_remote_image_to_server,
};
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
pub(crate) use handshake::probe_endpoint_negotiation;
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
use crate::protocol::{self, ClientMessage, FrameData, ServerMessage, MAX_GRAPHICS_FRAME_SIZE};
#[cfg(test)]
use crate::protocol::{AttachScrollDirection, AttachScrollSource, NotifyKind};
use crate::server::socket_paths::client_socket_path;

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
        loaded_config.config.kitty_graphics_enabled() && client_rendered_shell;
    let pixel_geometry_enabled = kitty_graphics_enabled || attach_escape.is_some();
    let endpoint_keybindings = shell_config
        .as_ref()
        .is_some_and(shell::ClientShellConfig::uses_endpoint_keybindings);
    let loop_config = ClientLoopConfig {
        sound_config: loaded_config.config.ui.sound,
        mouse_scroll_lines,
        redraw_on_focus_gained,
        host_cursor,
        kitty_graphics_enabled,
        pixel_geometry_enabled,
        pixel_geometry_fallback: kitty_graphics_enabled,
        mouse_capture_active: mouse_capture,
        endpoint_keybindings,
        remote_image_paste_key,
        shell_config,
    };

    crate::logging::startup("client");
    info!(path = %socket_path.display(), "{log_message}");

    let endpoint_catalog = if client_rendered_shell && !is_remote_client_process() {
        endpoint::EndpointCatalog::load().unwrap_or_else(|error| {
            warn!(%error, "saved SSH endpoint catalog is unavailable");
            endpoint::EndpointCatalog::default()
        })
    } else {
        endpoint::EndpointCatalog::default()
    };
    let federated = endpoint_catalog.has_enabled_ssh();

    let initial_stream = match crate::ipc::connect_local_stream(&socket_path) {
        Ok(stream) => Some(stream),
        Err(error) if federated => {
            warn!(%error, "Local is unavailable; keeping saved machines available");
            None
        }
        Err(error) => {
            return Err(io::Error::other(
                ClientError::ConnectionFailed(error).to_string(),
            ));
        }
    };

    // Get the terminal geometry before handshake (before raw mode).
    let (cols, rows, cell_width_px, cell_height_px, exact_cell_size) =
        initial_terminal_geometry(pixel_geometry_enabled, kitty_graphics_enabled)?;

    let shell_surface_size = loop_config
        .shell_config
        .as_ref()
        .map(|shell| shell.initial_surface_size(cols, rows));
    // Healthy Local attaches directly; only an actual failure enters background recovery.
    let initial = initial_stream
        .map(|mut stream| {
            let handshake = do_handshake(
                &mut stream,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                exact_cell_size,
                shell_surface_size,
                endpoint_keybindings,
                loop_config.mouse_capture_active,
                true,
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
            if federated
                && !endpoint::EndpointNegotiation::new(
                    handshake.endpoint_methods.clone().unwrap_or_default(),
                    handshake.endpoint_capabilities.clone().unwrap_or_default(),
                )
                .supports_surface_interest()
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Local needs a server update before it can participate in multi-machine viewing",
                ));
            }
            if let Some((terminal_id, takeover)) = attach_request {
                write_to_server(
                    &mut stream,
                    &ClientMessage::AttachTerminal {
                        terminal_id,
                        takeover,
                    },
                )?;
            }
            Ok((stream, handshake))
        })
        .transpose();
    let initial = match initial {
        Ok(initial) => initial,
        Err(error) if federated => {
            warn!(%error, "Local handshake failed; keeping saved machines available");
            None
        }
        Err(error) => return Err(error),
    };

    // The federated shell can show connection notices without any server snapshot.
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
            initial,
            endpoint_catalog,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            exact_cell_size,
            should_quit,
            loop_config,
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

/// The main client event loop.
///
/// Uses a threaded architecture:
/// - stdin reader thread → sends raw input bytes to main loop
/// - resize poller thread → sends resize events to main loop
/// - server reader thread → reads ServerMessages and sends to main loop
/// - main loop: coordinates input, output, and server communication
async fn run_client_loop(
    initial: Option<(LocalStream, handshake::HandshakeResult)>,
    mut endpoint_catalog: endpoint::EndpointCatalog,
    cols: u16,
    rows: u16,
    initial_cell_width_px: u32,
    initial_cell_height_px: u32,
    initial_pixel_geometry_exact: bool,
    should_quit: Arc<AtomicBool>,
    config: ClientLoopConfig,
    attach_escape: Option<AttachEscapeState>,
) -> Result<(), ClientError> {
    #[cfg(windows)]
    let _ = config.mouse_scroll_lines;
    let draw_host_cursor = attach_escape.is_none() && should_draw_host_cursor(config.host_cursor);
    let is_remote_client = is_remote_client_process();
    let local_unavailable = initial.is_none();

    let mut state = ClientState {
        blit_encoder: render_ansi::BlitEncoder::new(),
        mouse_capture_active: config.mouse_capture_active,
        endpoint_mouse_capture_requested: false,
        endpoint_sgr_pixels_requested: false,
        host_theme_updates: Vec::new(),
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
        presentation_frozen: false,
        draw_host_cursor,
        detached_process_children: Vec::new(),
        shell: config.shell_config.map(shell::ClientShellState::new),
    };
    let mut federated = endpoint_catalog.has_enabled_ssh();
    if let Some(shell) = state.shell.as_mut() {
        shell.set_graphics_cell_size(initial_cell_width_px, initial_cell_height_px);
        shell.set_endpoint_catalog(&endpoint_catalog.ssh);
        shell.set_endpoint_methods_for(
            &endpoint::ClientEndpointId::Local,
            initial
                .as_ref()
                .and_then(|(_, handshake)| handshake.endpoint_methods.clone()),
        );
        shell.set_endpoint_agent_view_projection_supported(
            &endpoint::ClientEndpointId::Local,
            initial
                .as_ref()
                .and_then(|(_, handshake)| handshake.endpoint_capabilities.as_ref())
                .is_some_and(|capabilities| {
                    capabilities.iter().any(|capability| {
                        capability == crate::protocol::endpoint::AGENT_VIEW_PROJECTION_CAPABILITY
                    })
                }),
        );
        if local_unavailable {
            shell.set_endpoint_status(
                &endpoint::ClientEndpointId::Local,
                endpoint::ClientEndpointStatus::Connecting,
            );
        }
    }
    let host_mouse_capture_active = Arc::new(AtomicBool::new(state.mouse_capture_active));
    // Cell size reported by the host terminal, packed as width<<32 | height.
    // Zero means the host has not reported one.
    let reported_cell_size = Arc::new(AtomicU64::new(0));
    let host_sgr_pixels_active = Arc::new(AtomicBool::new(false));

    // Channel for events from the resize and server reader threads.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);
    let (supervisor_tx, mut supervisor_rx) =
        tokio::sync::mpsc::channel::<endpoint::EndpointSupervisorEvent>(64);
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

    let mut write_stream = if let Some((stream, handshake)) = initial {
        let max_frame_size = if state.kitty_graphics_enabled {
            MAX_GRAPHICS_FRAME_SIZE
        } else {
            crate::protocol::MAX_FRAME_SIZE
        };
        let transport = start_endpoint_transport(
            stream,
            (),
            &event_tx,
            endpoint::ClientEndpointId::Local,
            1,
            max_frame_size,
        )?;
        let negotiation = endpoint::EndpointNegotiation::new(
            handshake.endpoint_methods.unwrap_or_default(),
            handshake.endpoint_capabilities.unwrap_or_default(),
        );
        let mut registry = endpoint::EndpointRegistry::new(transport, 1, negotiation);
        if state.shell.is_some() {
            registry.send(&ClientMessage::ClientShellFocus { focused: true });
        }
        registry
    } else {
        endpoint::EndpointRegistry::empty()
    };
    let mut supervisors =
        endpoint::EndpointSupervisors::new(&endpoint_catalog.ssh, std::time::Instant::now());
    if federated {
        supervisors.add_local(
            client_socket_path(),
            write_stream
                .connection(&endpoint::ClientEndpointId::Local)
                .map(|connection| connection.generation),
            std::time::Instant::now(),
        );
    }
    if local_unavailable {
        if let Some(frame) = state
            .shell
            .as_mut()
            .and_then(|shell| shell.compose(cols, rows))
        {
            state.present_frame(frame);
        }
    }
    let mut next_surface_serial = 1_u64;
    let mut pending_activation: Option<endpoint::PendingEndpointActivation> = None;
    let mut scheduled_activation = None;
    let mut pending_catalog: Option<Result<Vec<endpoint::SavedSshEndpoint>, String>> = None;
    if state.shell.is_some() && !is_remote_client && state.attach_escape.is_none() {
        catalog_reload::watch_profiles(event_tx.clone(), should_quit.clone());
    }

    // This (foreground) client owns the prefix ASCII input-source switch
    // (implemented on macOS and Windows; a no-op on other platforms).
    let mut prefix_input_source = crate::platform::RealPrefixInputSource::default();

    // Main event loop.
    let mut client_timer = timer::ClientLoopTimer::new();
    #[cfg(windows)]
    let mut stdin_open = true;
    while !should_quit.load(Ordering::Acquire) {
        if pending_activation.is_none() {
            if let Some(reload) = pending_catalog.take() {
                match reload {
                    Ok(profiles) => {
                        let now = std::time::Instant::now();
                        if !federated && profiles.iter().any(|profile| profile.enabled) {
                            // Keep Local recovery once enabled, even after removing the last SSH profile.
                            federated = true;
                            supervisors.add_local(
                                client_socket_path(),
                                write_stream
                                    .connection(&endpoint::ClientEndpointId::Local)
                                    .map(|connection| connection.generation),
                                now,
                            );
                            if write_stream
                                .connection(&endpoint::ClientEndpointId::Local)
                                .is_some_and(|connection| {
                                    !connection.negotiation.supports_surface_interest()
                                })
                            {
                                if let Some(shell) = state.shell.as_mut() {
                                    shell.receive_endpoint_unavailable(
                                        "Update the Local server before switching between machines"
                                            .into(),
                                    );
                                }
                            }
                        }
                        let active_removed = catalog_reload::apply_profiles(
                            &mut state,
                            &mut write_stream,
                            &mut endpoint_commands,
                            &mut supervisors,
                            &mut endpoint_catalog,
                            profiles,
                            now,
                        );
                        if active_removed {
                            clear_endpoint_host_effects(
                                &mut state,
                                &host_mouse_capture_active,
                                &host_sgr_pixels_active,
                            );
                            scheduled_activation = None;
                            if state.shell.as_ref().is_some_and(|shell| {
                                shell.endpoint_projection_available(
                                    &endpoint::ClientEndpointId::Local,
                                )
                            }) && write_stream
                                .connection(&endpoint::ClientEndpointId::Local)
                                .is_some()
                            {
                                scheduled_activation = Some(ClientLoopEvent::ActivateEndpoint {
                                    endpoint_id: endpoint::ClientEndpointId::Local,
                                    target: None,
                                    force: true,
                                });
                            } else {
                                present_handoff_unavailable(
                                    &mut state,
                                    "Local is unavailable; reconnecting".into(),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        warn!(%error, "saved machines could not be reloaded; keeping current connections");
                        if let Some(shell) = state.shell.as_mut() {
                            shell.receive_endpoint_unavailable(format!(
                                "Saved machines could not be reloaded; keeping current connections: {error}"
                            ));
                        }
                    }
                }
                apply_client_shell_input_source_changes(&mut state, &mut prefix_input_source);
                if let Some(shell) = state.shell.as_mut() {
                    let cleanup = shell.take_pending_graphics_cleanup();
                    let frame = shell.compose(state.reported_size.0, state.reported_size.1);
                    let frozen = state.presentation_frozen;
                    state.presentation_frozen = false;
                    state.present_graphics(&cleanup);
                    if let Some(frame) = frame {
                        state.present_frame(frame);
                    }
                    state.presentation_frozen = frozen;
                }
            }
        }
        if let Some(shell) = state.shell.as_ref() {
            supervisors.spawn_due(
                std::time::Instant::now(),
                endpoint::EndpointConnectOptions {
                    cols: state.reported_size.0,
                    rows: state.reported_size.1,
                    cell_width_px: state.reported_cell_size.0,
                    cell_height_px: state.reported_cell_size.1,
                    pixel_geometry_exact: state.pixel_geometry_exact,
                    surface_size: shell.surface_size(state.reported_size.0, state.reported_size.1),
                    endpoint_keybindings: config.endpoint_keybindings,
                    mouse_capture: state.shell_mouse_capture_preference,
                },
                &supervisor_tx,
            );
        }
        let timer_delay = state
            .shell
            .as_ref()
            .map_or(Duration::from_millis(100), |shell| {
                shell.timer_delay(std::time::Instant::now())
            });
        let timer_deadline = client_timer.deadline(std::time::Instant::now(), timer_delay);
        let immediate_event = scheduled_activation.take();
        #[cfg(windows)]
        let event = if let Some(event) = immediate_event {
            event
        } else {
            tokio::select! {
                _ = tokio::time::sleep_until(timer_deadline.into()) => ClientLoopEvent::Timer,
                ev = stdin_rx.recv(), if stdin_open => match ev {
                    Some(event) => event,
                    None => {
                        stdin_open = false;
                        ClientLoopEvent::Timer
                    }
                },
                ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
                ev = supervisor_rx.recv() => ev.map(ClientLoopEvent::EndpointSupervisor).unwrap_or(ClientLoopEvent::Timer),
            }
        };
        #[cfg(unix)]
        let event = if let Some(event) = immediate_event {
            event
        } else {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(timer_deadline.into()) => ClientLoopEvent::Timer,
                ev = supervisor_rx.recv() => ev.map(ClientLoopEvent::EndpointSupervisor).unwrap_or(ClientLoopEvent::Timer),
                ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
            }
        };
        let now = std::time::Instant::now();
        if let Some(shell) = state.shell.as_mut() {
            shell.tick_popup_pending(now);
        }

        match event {
            ClientLoopEvent::EndpointCatalog(reload) => pending_catalog = Some(reload),
            #[cfg(unix)]
            ClientLoopEvent::StdinInput(data) => {
                let image_bridge_active = endpoint_accepts_local_images(
                    is_remote_client,
                    write_stream.active_id(),
                    write_stream.active_surface_available(),
                );
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
                            image_bridge_active,
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
                            read_image_file_from_terminal_drop(&data, image_bridge_active)
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
                        &mut pending_activation,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                        &event_tx,
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
                    image_bridge_active,
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
                if let Some(image) = read_image_file_from_terminal_drop(&data, image_bridge_active)
                {
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
                let pending_key = state
                    .pending_surface_graphics
                    .keys()
                    .find(|(_, transfer_id, image_id)| {
                        *transfer_id == response.transfer_id && *image_id == response.image_id
                    })
                    .cloned();
                let owner = pending_key
                    .as_ref()
                    .map(|(endpoint_id, _, _)| endpoint_id.clone());
                let composed = pending_key
                    .and_then(|key| {
                        state
                            .pending_surface_graphics
                            .remove(&key)
                            .map(|asset| (key, asset))
                    })
                    .filter(|_| response.success)
                    .and_then(|((endpoint_id, _, _), asset)| {
                        if write_stream.active_id() != &endpoint_id {
                            return None;
                        }
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
                if let Some(owner) = owner {
                    write_stream.send_to(&owner, &message);
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
                        &mut pending_activation,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                        &event_tx,
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
                let image_bridge_active = endpoint_accepts_local_images(
                    is_remote_client,
                    write_stream.active_id(),
                    write_stream.active_surface_available(),
                );
                if state.shell.is_some() {
                    let image_target = state
                        .shell
                        .as_ref()
                        .and_then(|shell| shell.clipboard_image_target());
                    if let Some(target) = image_target.clone() {
                        if should_bridge_clipboard_image_events(
                            &events,
                            image_bridge_active,
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
                            read_image_file_from_client_events(&events, image_bridge_active)
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
                        &mut pending_activation,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                        &event_tx,
                    )? {
                        return Ok(());
                    }
                    continue;
                }
                // Direct terminal attach is Unix-only; every Windows client uses ClientShell.
            }
            ClientLoopEvent::TerminalUnavailable(err) => {
                info!(err = %err, "client terminal unavailable; detaching");
                let _ = write_to_server(&mut write_stream, &ClientMessage::Detach);
                return Ok(());
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
                if let Some(activation) = pending_activation.as_mut() {
                    if let Err(error) = activation.update_resize(msg, &mut write_stream) {
                        rollback_endpoint_activation(
                            &mut state,
                            &mut write_stream,
                            &mut pending_activation,
                            error,
                            false,
                        );
                    }
                } else if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::EndpointSupervisor(event) => match event {
                endpoint::EndpointSupervisorEvent::Status {
                    endpoint_id,
                    generation,
                    status,
                    message,
                } => {
                    if !supervisors.record_status(&endpoint_id, generation, status, now) {
                        continue;
                    }
                    if status == endpoint::ClientEndpointStatus::Attention {
                        warn!(endpoint = %endpoint_id.storage_key(), generation, error = %message, "endpoint needs attention");
                    }
                    let unavailable = state.shell.as_mut().and_then(|shell| {
                        shell.set_endpoint_status(&endpoint_id, status);
                        (status == endpoint::ClientEndpointStatus::Attention
                            && shell.endpoint_is_active(&endpoint_id))
                        .then(|| format!("{}: {message}", shell.endpoint_label(&endpoint_id)))
                    });
                    if let Some(message) = unavailable {
                        present_handoff_unavailable(&mut state, message);
                    } else if let Some(frame) = state.shell.as_mut().and_then(|shell| {
                        shell.compose(state.reported_size.0, state.reported_size.1)
                    }) {
                        state.present_frame(frame);
                    }
                }
                endpoint::EndpointSupervisorEvent::Connected {
                    endpoint_id,
                    generation,
                    reader,
                    writer,
                    negotiation,
                } => {
                    if !supervisors.record_status(
                        &endpoint_id,
                        generation,
                        endpoint::ClientEndpointStatus::Online,
                        now,
                    ) {
                        continue;
                    }
                    let agent_view_projection_supported = negotiation.supports_capability(
                        crate::protocol::endpoint::AGENT_VIEW_PROJECTION_CAPABILITY,
                    );
                    let frame = state.shell.as_mut().and_then(|shell| {
                        shell.set_endpoint_methods_for(&endpoint_id, Some(negotiation.methods()));
                        shell.set_endpoint_agent_view_projection_supported(
                            &endpoint_id,
                            agent_view_projection_supported,
                        );
                        shell.compose(state.reported_size.0, state.reported_size.1)
                    });
                    let reader_quit = writer.stop_handle();
                    write_stream.insert(
                        endpoint_id.clone(),
                        writer,
                        generation,
                        negotiation,
                        false,
                    );
                    if let Some(frame) = frame {
                        state.present_frame(frame);
                    }
                    let reader_tx = event_tx.clone();
                    std::thread::spawn(move || {
                        server_reader_thread(
                            reader,
                            reader_tx,
                            &reader_quit,
                            MAX_GRAPHICS_FRAME_SIZE,
                            endpoint_id,
                            generation,
                        );
                    });
                }
            },
            ClientLoopEvent::ActivateEndpoint {
                endpoint_id,
                target,
                force,
            } => {
                if !endpoint_catalog.select_endpoint(&endpoint_id) {
                    continue;
                }
                if let Err(error) = endpoint_catalog.store_selection() {
                    warn!(%error, "failed to persist desired endpoint selection");
                }
                begin_endpoint_activation(
                    &mut state,
                    &mut write_stream,
                    &mut endpoint_commands,
                    &mut pending_activation,
                    &mut next_surface_serial,
                    endpoint_id,
                    target,
                    force,
                    now,
                    &event_tx,
                )?;
            }
            ClientLoopEvent::ServerMessage {
                endpoint_id,
                generation,
                message,
            } => {
                if !write_stream.accepts(&endpoint_id, generation) {
                    continue;
                }
                write_stream.received(&endpoint_id, generation, now);
                let endpoint_active = write_stream.active_id() == &endpoint_id
                    && write_stream
                        .connection(&endpoint_id)
                        .is_some_and(|connection| connection.surface_active);
                let activation_message = pending_activation
                    .as_ref()
                    .is_some_and(|pending| pending.accepts_endpoint(&endpoint_id, generation));
                let command_response = match message.as_ref() {
                    ServerMessage::ClientShellEndpointResponseChunk {
                        boot_id,
                        request_id,
                        ..
                    } => endpoint_commands.accepts_response(
                        &endpoint_id,
                        generation,
                        boot_id,
                        request_id,
                    ),
                    _ => false,
                };
                if !endpoint::accepts_endpoint_message(
                    endpoint_active,
                    activation_message,
                    command_response,
                    message.as_ref(),
                ) {
                    continue;
                }
                // Target presentation effects may arrive as soon as surface.set(true) is
                // acknowledged. They cannot be applied while the source frame is frozen; the
                // target receives one explicit replay after the coherent commit instead.
                if state.presentation_frozen
                    && activation_message
                    && endpoint::is_presentation_effect(message.as_ref())
                {
                    continue;
                }
                match *message {
                    ServerMessage::ClientShellSnapshot(_) => {
                        let message = "server sent an unnegotiated binary endpoint snapshot";
                        if federated || !endpoint_id.is_local() {
                            if handle_endpoint_attention(
                                &mut state,
                                &mut write_stream,
                                &mut endpoint_commands,
                                &mut supervisors,
                                &mut pending_activation,
                                &endpoint_id,
                                generation,
                                now,
                                message.into(),
                            ) {
                                clear_endpoint_host_effects(
                                    &mut state,
                                    &host_mouse_capture_active,
                                    &host_sgr_pixels_active,
                                );
                            }
                            continue;
                        }
                        return Err(ClientError::Protocol(protocol::FramingError::Io(
                            io::Error::new(io::ErrorKind::InvalidData, message),
                        )));
                    }
                    ServerMessage::PaneSurface(surface) => {
                        if activation_message {
                            let progress = pending_activation.as_mut().map(|pending| {
                                pending.receive_surface(&endpoint_id, generation, surface)
                            });
                            if matches!(progress, Some(endpoint::SurfaceActivationProgress::Ready))
                            {
                                if let Some(event) = complete_endpoint_activation(
                                    &mut state,
                                    &mut write_stream,
                                    &mut pending_activation,
                                    &mut endpoint_commands,
                                )? {
                                    scheduled_activation = Some(event);
                                }
                            }
                            continue;
                        }
                        if !endpoint_active {
                            continue;
                        }
                        let composed = if let Some(shell) = &mut state.shell {
                            shell.set_pane_surface(surface);
                            shell.compose(state.reported_size.0, state.reported_size.1)
                        } else {
                            None
                        };
                        apply_client_shell_input_source_changes(
                            &mut state,
                            &mut prefix_input_source,
                        );
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
                        crate::render_prof::duration_since(
                            "client_surface_patch.apply",
                            apply_started,
                        );
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
                        apply_client_shell_input_source_changes(
                            &mut state,
                            &mut prefix_input_source,
                        );
                        if compose_fallback {
                            let composed = state.shell.as_mut().and_then(|shell| {
                                shell.compose(state.reported_size.0, state.reported_size.1)
                            });
                            if let Some(frame) = composed {
                                state.present_frame(frame);
                            }
                        }
                        crate::render_prof::duration_since(
                            "client_surface_patch.total",
                            patch_started,
                        );
                        crate::render_prof::flush_if_due();
                    }
                    ServerMessage::Terminal(frame) => {
                        if state.kitty_graphics_enabled
                            && contains_kitty_graphics_bytes(&frame.bytes)
                        {
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
                            if state.retired_direct_graphics.take()
                                == Some((endpoint_id.clone(), transfer_id, image_id))
                            {
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
                                    state.pending_surface_graphics.insert(
                                        (endpoint_id.clone(), transfer_id, image_id),
                                        asset,
                                    );
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
                                state.pending_surface_graphics.remove(&(
                                    endpoint_id.clone(),
                                    transfer_id,
                                    image_id,
                                ));
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
                            state.retired_direct_graphics =
                                Some((endpoint_id.clone(), transfer_id, image_id));
                            state.pending_surface_graphics.remove(&(
                                endpoint_id.clone(),
                                transfer_id,
                                image_id,
                            ));
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
                        if !federated && endpoint_id.is_local() {
                            return Err(ClientError::ServerShutdown { reason });
                        }
                        write_stream.fail(
                            &endpoint_id,
                            io::Error::new(
                                io::ErrorKind::ConnectionAborted,
                                reason.unwrap_or_else(|| "server stopped".into()),
                            ),
                        );
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
                                let (effects, repaint) = shell.receive_notification(
                                    &endpoint_id,
                                    event,
                                    std::time::Instant::now(),
                                );
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
                                let frame =
                                    shell.compose(state.reported_size.0, state.reported_size.1);
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
                        if pending_activation.as_ref().is_some_and(|pending| {
                            pending.accepts_response(
                                &endpoint_id,
                                generation,
                                &boot_id,
                                &request_id,
                            )
                        }) {
                            if !final_chunk {
                                rollback_endpoint_activation(
                                    &mut state,
                                    &mut write_stream,
                                    &mut pending_activation,
                                    "endpoint returned a chunked activation acknowledgement".into(),
                                    false,
                                );
                                continue;
                            }
                            let progress = pending_activation.as_mut().map(|pending| {
                                pending.receive_response_for_boot(
                                    &endpoint_id,
                                    generation,
                                    &boot_id,
                                    &request_id,
                                    &data,
                                    &mut write_stream,
                                )
                            });
                            match progress {
                                Some(endpoint::SurfaceActivationProgress::Ready) => {
                                    if let Some(event) = complete_endpoint_activation(
                                        &mut state,
                                        &mut write_stream,
                                        &mut pending_activation,
                                        &mut endpoint_commands,
                                    )? {
                                        scheduled_activation = Some(event);
                                    }
                                }
                                Some(endpoint::SurfaceActivationProgress::Rejected {
                                    message,
                                    source_release_rejected,
                                }) => {
                                    rollback_endpoint_activation(
                                        &mut state,
                                        &mut write_stream,
                                        &mut pending_activation,
                                        message,
                                        source_release_rejected,
                                    );
                                }
                                _ => {}
                            }
                            continue;
                        }
                        if request_id.starts_with("client-shell-surface:") {
                            continue;
                        }
                        let completed = endpoint_commands
                            .receive_chunk(
                                &endpoint_id,
                                generation,
                                &boot_id,
                                &request_id,
                                final_chunk,
                                data,
                            )
                            .map_err(ClientError::ConnectionLost)?;
                        let Some(completed) = completed else {
                            continue;
                        };
                        let (repaint, actions) = state.shell.as_mut().map_or_else(
                            || (false, Vec::new()),
                            |shell| {
                                if completed.generation == generation
                                    && shell.endpoint_is_active(&completed.endpoint_id)
                                {
                                    shell.handle_endpoint_result(
                                        &completed.boot_id,
                                        &completed.request_id,
                                        completed.result,
                                    )
                                } else {
                                    (
                                        shell.cancel_endpoint_request(&completed.request_id),
                                        Vec::new(),
                                    )
                                }
                            },
                        );
                        if let Some(shell) = state.shell.as_mut() {
                            shell.reconcile_input_source();
                        }
                        apply_client_shell_input_source_changes(
                            &mut state,
                            &mut prefix_input_source,
                        );
                        let (replay_mouse, dispatch_repaint) = dispatch_client_shell_actions(
                            actions,
                            &mut endpoint_commands,
                            &mut write_stream,
                            state.shell.as_mut(),
                            &mut state.detached_process_children,
                            &event_tx,
                        )?;
                        let repaint = repaint || dispatch_repaint;
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
                                &mut pending_activation,
                                &mut endpoint_commands,
                                &mut prefix_input_source,
                                &event_tx,
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
                    ServerMessage::ReloadSoundConfig => apply_reload(
                        &mut state,
                        &mut write_stream,
                        &mut pending_activation,
                        &host_mouse_capture_active,
                        &host_sgr_pixels_active,
                        &mut prefix_input_source,
                    )?,
                    ServerMessage::MouseCapture {
                        enabled,
                        sgr_pixels,
                    } => {
                        state.endpoint_mouse_capture_requested = enabled;
                        state.endpoint_sgr_pixels_requested = sgr_pixels;
                        let enabled =
                            effective_mouse_capture(enabled, state.direct_mouse_capture_preference);
                        let next_sgr_pixels = effective_sgr_pixel_mouse(
                            enabled,
                            sgr_pixels,
                            state.pixel_geometry_exact,
                        );
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
                        if kind == crate::protocol::endpoint::PRESENTATION_EFFECTS_READY_KIND {
                            let progress = pending_activation.as_mut().map(|activation| {
                                activation.receive_presentation_effects_ready(
                                    &endpoint_id,
                                    generation,
                                    &data,
                                )
                            });
                            if matches!(progress, Some(endpoint::SurfaceActivationProgress::Ready))
                            {
                                if let Some(event) = complete_endpoint_activation(
                                    &mut state,
                                    &mut write_stream,
                                    &mut pending_activation,
                                    &mut endpoint_commands,
                                )? {
                                    scheduled_activation = Some(event);
                                }
                            }
                            continue;
                        }
                        let snapshot = match endpoint::decode_endpoint_control(&kind, &data) {
                            Ok(endpoint::EndpointControlMessage::HealthPong) => continue,
                            Ok(endpoint::EndpointControlMessage::AgentViewProjection(
                                projection,
                            )) => {
                                if let Some(shell) = state.shell.as_mut() {
                                    shell.set_endpoint_agent_view_projection_for_generation(
                                        &endpoint_id,
                                        generation,
                                        projection,
                                    );
                                }
                                continue;
                            }
                            Ok(endpoint::EndpointControlMessage::Ignored) => {
                                debug!(%kind, "ignoring unknown endpoint control message");
                                continue;
                            }
                            Ok(endpoint::EndpointControlMessage::Snapshot(snapshot)) => snapshot,
                            Err(message)
                                if federated
                                    || !endpoint::protocol_failure_is_fatal(&endpoint_id) =>
                            {
                                if handle_endpoint_attention(
                                    &mut state,
                                    &mut write_stream,
                                    &mut endpoint_commands,
                                    &mut supervisors,
                                    &mut pending_activation,
                                    &endpoint_id,
                                    generation,
                                    now,
                                    message,
                                ) {
                                    clear_endpoint_host_effects(
                                        &mut state,
                                        &host_mouse_capture_active,
                                        &host_sgr_pixels_active,
                                    );
                                }
                                continue;
                            }
                            Err(message) => {
                                return Err(ClientError::Protocol(protocol::FramingError::Io(
                                    io::Error::new(io::ErrorKind::InvalidData, message),
                                )));
                            }
                        };
                        let projection_pending = activation_message;
                        let activation_progress = activation_message
                            .then(|| {
                                pending_activation.as_mut().map(|pending| {
                                    pending.receive_snapshot(&endpoint_id, generation, &snapshot)
                                })
                            })
                            .flatten();
                        install_client_shell_snapshot(
                            &mut state,
                            &endpoint_id,
                            snapshot,
                            projection_pending,
                            &mut write_stream,
                            &mut prefix_input_source,
                        )?;
                        if matches!(
                            activation_progress,
                            Some(endpoint::SurfaceActivationProgress::Ready)
                        ) {
                            if let Some(event) = complete_endpoint_activation(
                                &mut state,
                                &mut write_stream,
                                &mut pending_activation,
                                &mut endpoint_commands,
                            )? {
                                scheduled_activation = Some(event);
                            }
                        }
                        write_stream.mark_ready(&endpoint_id, generation);
                        let selected_endpoint = endpoint_catalog
                            .selected_profile
                            .as_ref()
                            .map_or(endpoint::ClientEndpointId::Local, |profile_id| {
                                endpoint::ClientEndpointId::Ssh(profile_id.clone())
                            });
                        let activation_ready = state.shell.as_ref().is_some_and(|shell| {
                            shell.endpoint_has_snapshot(&selected_endpoint)
                                && (!write_stream
                                    .connection(write_stream.active_id())
                                    .is_some_and(|connection| connection.surface_active)
                                    || shell.endpoint_boot_id(write_stream.active_id()).is_some())
                        });
                        let needs_surface = write_stream
                            .connection(&selected_endpoint)
                            .is_some_and(|connection| !connection.surface_active);
                        if activation_ready && needs_surface && pending_activation.is_none() {
                            scheduled_activation = Some(ClientLoopEvent::ActivateEndpoint {
                                endpoint_id: selected_endpoint,
                                target: None,
                                force: false,
                            });
                        }
                    }
                    ServerMessage::Welcome { .. } => {
                        debug!("received unexpected Welcome in main loop");
                    }
                }
            }
            ClientLoopEvent::ServerDisconnected {
                endpoint_id,
                generation,
            } => {
                if !write_stream.accepts(&endpoint_id, generation) {
                    continue;
                }
                write_stream.fail(
                    &endpoint_id,
                    io::Error::new(io::ErrorKind::UnexpectedEof, "connection was lost"),
                );
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
                write_stream.tick_health(now);
                for failure in write_stream.take_failures() {
                    if write_stream.connection(&failure.endpoint_id).is_some()
                        && !write_stream.accepts(&failure.endpoint_id, failure.generation)
                    {
                        continue;
                    }
                    warn!(
                        endpoint = %failure.endpoint_id.storage_key(),
                        error = %failure.message,
                        "endpoint transport failed"
                    );
                    if !federated && failure.endpoint_id.is_local() {
                        return Err(ClientError::ConnectionLost(io::Error::new(
                            failure.kind,
                            failure.message,
                        )));
                    }
                    if handle_endpoint_disconnect(
                        &mut state,
                        &mut write_stream,
                        &mut endpoint_commands,
                        &mut supervisors,
                        &mut pending_activation,
                        &failure.endpoint_id,
                        failure.generation,
                        now,
                        &format!("{}; reconnecting", failure.message),
                    ) {
                        clear_endpoint_host_effects(
                            &mut state,
                            &host_mouse_capture_active,
                            &host_sgr_pixels_active,
                        );
                    }
                }
                // A revoked transport changes the safe rollback destination. Handle those
                // failures before applying a timeout to the remaining activation phase.
                if pending_activation
                    .as_ref()
                    .is_some_and(|activation| activation.expired(now))
                {
                    let endpoint_id = pending_activation
                        .as_ref()
                        .map(|activation| activation.target().clone())
                        .expect("checked pending activation");
                    let label = state
                        .shell
                        .as_ref()
                        .map(|shell| shell.endpoint_label(&endpoint_id).to_owned())
                        .unwrap_or_else(|| "Endpoint".into());
                    rollback_endpoint_activation(
                        &mut state,
                        &mut write_stream,
                        &mut pending_activation,
                        format!("{label} did not produce a coherent surface in time"),
                        false,
                    );
                }
                if state.shell.is_some() {
                    let expired_endpoints = endpoint_commands
                        .expire(now)
                        .into_iter()
                        .filter(|expired| {
                            write_stream.accepts(&expired.endpoint_id, expired.generation)
                        })
                        .collect::<Vec<_>>();
                    let (effects, outcome, frame) = {
                        let shell = state.shell.as_mut().expect("checked shell mode");
                        let mut outcome = shell.tick_selection_autoscroll(now);
                        for expired in expired_endpoints {
                            if !shell.endpoint_is_active(&expired.endpoint_id) {
                                continue;
                            }
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
                        &mut pending_activation,
                        &mut endpoint_commands,
                        &mut prefix_input_source,
                        &event_tx,
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

#[cfg(test)]
mod tests;
