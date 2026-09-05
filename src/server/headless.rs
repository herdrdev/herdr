//! Headless server mode — runs the herdr event loop without a real terminal.
//!
//! The server:
//! - Does not enter raw mode or read stdin
//! - Creates and listens on both `herdr.sock` (existing JSON API) and
//!   `herdr-client.sock` (new binary protocol)
//! - Initializes AppState and all PTYs from session restore or fresh state
//! - Runs the main event loop (drain events, drain API requests, scheduled tasks)
//! - Renders to a virtual ratatui Buffer in memory
//! - Accepts client connections on the client socket
//! - Streams frames to connected clients after each render
//! - Routes client input events through the existing input pipeline
//! - Continues running after client disconnect
//! - Handles stale socket cleanup, explicit server stop, minimum terminal size,
//!   and pane spawn failure during restore

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use interprocess::local_socket::traits::Listener as _;
#[cfg(windows)]
use interprocess::local_socket::traits::Stream as _;
#[cfg(unix)]
use interprocess::local_socket::ListenerNonblockingMode;
use ratatui::layout::Rect;
use tokio::sync::mpsc;
#[cfg(windows)]
use tracing::error;
use tracing::{debug, info, warn};

use base64::Engine;
use bytes::Bytes;

use crate::api;
use crate::app;
use crate::config;
use crate::events::AppEvent;
use crate::ipc::{
    bind_private_local_listener, remove_socket_file_if_owned, socket_file_identity, LocalListener,
    SocketFileIdentity,
};
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, FrameData, ServerMessage, MAX_FRAME_SIZE,
};
#[cfg(unix)]
use crate::server::client_accept::{
    accept_pending_client_connections, reject_pending_client_connections,
};
use crate::server::client_shell::{
    render_pane_surface as render_client_shell_pane_surface, snapshot as client_shell_snapshot,
};
use crate::server::client_transport::ServerEvent;
use crate::server::clients::{
    latest_shell_client, render_targets, terminal_stream_client_ids, ClientConnection,
    ClientConnectionMode, ClientShellInputTarget, DeferredRender,
};
use crate::server::keybindings::{app_keybindings, apply_keybindings};
use crate::server::notifications::{
    should_forward_toast_to_clients, toast_message_from_state_change, toast_notify_kind,
};
use crate::server::pane_input::{
    apply_client_pane_input_events, apply_client_popup_input_events, apply_terminal_attach_input,
    apply_terminal_attach_scroll, terminal_attach_mouse_position,
};
use crate::server::socket_paths::{client_socket_path, prepare_socket_path};
use crate::server::terminal_attach::paste_payload_for_runtime;

mod bootstrap;
mod client_views;
mod lifecycle;
mod notifications;
mod pane_graphics;
mod render;
mod retained_surface;

pub use bootstrap::run_server;
use lifecycle::wait_for_live_handoff_response_write;
#[cfg(unix)]
use lifecycle::wait_for_old_public_sockets_to_close;

use crate::protocol::MAX_GRAPHICS_FRAME_SIZE;

#[cfg(test)]
use crate::protocol::RenderEncoding;
#[cfg(test)]
use crate::server::client_transport::ClientWriter;
#[cfg(test)]
use std::fs;

fn sound_notify_message(sound: crate::sound::Sound) -> &'static str {
    match sound {
        crate::sound::Sound::Done => "agent done",
        crate::sound::Sound::Request => "agent attention",
    }
}

fn notification_show_result(
    id: String,
    shown: bool,
    reason: api::schema::NotificationShowReason,
) -> String {
    serde_json::to_string(&api::schema::SuccessResponse {
        id,
        result: api::schema::ResponseResult::NotificationShow { shown, reason },
    })
    .unwrap_or_else(|_| "{}".to_string())
}

fn non_empty_body(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

// ---------------------------------------------------------------------------
// Loop event enum for the headless server event loop
// ---------------------------------------------------------------------------

/// Events that the headless server event loop can process.
enum LoopEvent {
    Timer,
    Internal(AppEvent),
    Api(Box<api::ApiRequestMessage>),
    ServerEvent(ServerEvent),
    RenderRequested,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum RenderImpact {
    #[default]
    None,
    Graphics,
    Full,
}

impl RenderImpact {
    fn merge(&mut self, other: Self) {
        *self = (*self).max(other);
    }
}

fn record_render_impact(source: &'static str, impact: RenderImpact) {
    let event = match (source, impact) {
        ("api_requests", RenderImpact::Graphics) => "graphics_render_cause.api_requests",
        ("api_requests", RenderImpact::Full) => "full_render_cause.api_requests",
        ("server_events", RenderImpact::Graphics) => "graphics_render_cause.server_events",
        ("server_events", RenderImpact::Full) => "full_render_cause.server_events",
        _ => return,
    };
    crate::render_prof::event(event);
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Timeout for in-flight API requests during shutdown.
#[allow(dead_code)]
const SHUTDOWN_API_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the idle headless loop wakes to poll the local listener for new
/// client connections.
///
/// The listener is non-blocking and not integrated into `tokio::select!`, so
/// a low-frequency wake is required to notice new thin-client attaches while
/// otherwise idle. Keep this much slower than the old resize-poll cadence to
/// avoid reintroducing the idle CPU spin.
const CLIENT_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Headless server
// ---------------------------------------------------------------------------

struct AltScreenReadSpec {
    terminal_id: crate::terminal::TerminalId,
    lines: usize,
    unwrap: bool,
    initial: crate::terminal::ScreenSnapshot,
    content_seq: u64,
}

enum AltScreenReadConflict {
    None,
    Frozen(crate::pane::TerminalReadSnapshot),
    Defer,
}

/// The headless server — runs the herdr event loop without a real terminal.
pub struct HeadlessServer {
    app: app::App,
    #[cfg(unix)]
    api_tx: Option<api::ApiRequestSender>,
    // Kept on every platform so dropping HeadlessServer owns API server shutdown.
    #[cfg_attr(windows, allow(dead_code))]
    api_server: Option<api::ServerHandle>,
    #[cfg(unix)]
    client_listener: LocalListener,
    client_socket_path: PathBuf,
    client_socket_identity: SocketFileIdentity,
    clients: HashMap<u64, ClientConnection>,
    #[cfg(unix)]
    next_client_id: u64,
    /// The client currently driving session-wide host presentation and side effects.
    foreground_client_id: Option<u64>,
    /// Ephemeral shell connection controlling PTY geometry for each stable tab id.
    tab_geometry_controllers: HashMap<String, u64>,
    /// Stable tab id whose viewers may see and interact with the one terminal popup.
    popup_owner_tab_id: Option<String>,
    /// Process-local identity used to reject shell replacements from an earlier server boot.
    client_shell_boot_id: String,
    /// Outer window title last pushed, paired with the client that received it.
    /// Keying on the client means a newly attached terminal is written to even
    /// when the title itself has not changed, without every code path that
    /// changes the foreground client having to remember to invalidate this.
    sent_window_title: Option<(u64, Option<String>)>,
    /// Window title set through `client.window_title.set`. While present it wins
    /// over the configured `ui.window_title` until the API clears it again.
    api_window_title: Option<String>,
    /// Server-owned keybindings, restored when foreground clients use server mode.
    server_keybindings: crate::config::LiveKeybindConfig,
    /// Full server config warning shown to clients that use server keybindings.
    server_config_diagnostic: Option<String>,
    /// Server config warning with keybinding diagnostics removed for local-keybinding clients.
    server_config_diagnostic_without_keybindings: Option<String>,
    /// Writable direct attach owner per terminal id string.
    terminal_attach_owners: HashMap<String, u64>,
    /// Deferred application-history reads currently driving alternate-screen viewports.
    pending_alt_screen_reads: Vec<crate::server::alt_screen_read::PendingAltScreenRead>,
    /// Reads waiting for an alternate-screen traversal of the same terminal to finish.
    deferred_alt_screen_reads: Vec<api::ApiRequestMessage>,
    /// Monotonic activity counter used to pick the most recently active client.
    next_activity_stamp: u64,
    /// Configured virtual terminal size used when no clients are connected.
    headless_size: (u16, u16),
    /// Shared pane runtime size derived from the foreground client, or the
    /// configured headless size when no clients are connected.
    effective_size: (u16, u16),
    /// Flag set when shutdown is initiated.
    shutting_down: bool,
    /// Flag set while exporting live PTYs to a replacement server.
    handoff_in_progress: bool,
    /// Imported panes get one app-safe resize nudge after the first client attaches.
    #[cfg(unix)]
    pending_handoff_repaint_nudge: bool,
    /// Flag set by Ctrl+C or `server stop` signal.
    should_quit: Arc<AtomicBool>,
    /// Channel for receiving server events from client connection threads.
    server_event_rx: mpsc::Receiver<ServerEvent>,
    /// Sender for server events (cloned for each client thread).
    server_event_tx: mpsc::Sender<ServerEvent>,
}

#[cfg(windows)]
fn spawn_windows_client_accept_thread(
    listener: LocalListener,
    should_quit: Arc<AtomicBool>,
    server_event_tx: mpsc::Sender<ServerEvent>,
) {
    std::thread::spawn(move || {
        let mut next_client_id = 1_u64;
        while !should_quit.load(Ordering::Acquire) {
            let stream = match listener.accept() {
                Ok(stream) => stream,
                Err(err) => {
                    if should_quit.load(Ordering::Acquire) {
                        break;
                    }
                    error!(err = %err, "client listener accept failed");
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };

            let client_id = next_client_id;
            next_client_id = next_client_id.saturating_add(1);

            if let Err(err) = stream.set_nonblocking(true) {
                warn!(err = %err, "failed to set client stream nonblocking");
                continue;
            }

            let should_quit = should_quit.clone();
            let server_event_tx = server_event_tx.clone();
            std::thread::spawn(move || {
                if let Err(err) = crate::server::client_transport::handle_client_handshake(
                    stream,
                    client_id,
                    &server_event_tx,
                    &should_quit,
                ) {
                    debug!(client_id, err = %err, "client handshake failed");
                }
            });
        }
    });
}

impl HeadlessServer {
    /// Creates and starts the headless server.
    ///
    /// This:
    /// 1. Prepares the client socket path (cleans up stale sockets)
    /// 2. Binds the client socket listener
    /// 3. Returns the server ready to run
    pub fn new(
        app: app::App,
        config_diagnostics: &[String],
        api_tx: Option<api::ApiRequestSender>,
        api_server: Option<api::ServerHandle>,
        should_quit: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let client_path = client_socket_path();
        prepare_socket_path(&client_path)?;

        let listener = bind_private_local_listener(&client_path)?;
        let client_socket_identity = socket_file_identity(&client_path)?;
        info!(path = %client_path.display(), "client protocol socket listening");

        // Set non-blocking on Unix so we can poll it from the event loop.
        #[cfg(unix)]
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

        // Channel for server events from client threads.
        let (server_event_tx, server_event_rx) = mpsc::channel(64);
        #[cfg(windows)]
        spawn_windows_client_accept_thread(listener, should_quit.clone(), server_event_tx.clone());

        let server_keybindings = app_keybindings(&app);
        let headless_size = app.state.headless_size;
        let (server_config_diagnostic, server_config_diagnostic_without_keybindings) =
            server_config_diagnostic_summaries(config_diagnostics);
        #[cfg(not(unix))]
        let _ = api_tx;
        Ok(Self {
            app,
            #[cfg(unix)]
            api_tx,
            api_server,
            #[cfg(unix)]
            client_listener: listener,
            client_socket_path: client_path,
            client_socket_identity,
            clients: HashMap::new(),
            #[cfg(unix)]
            next_client_id: 1,
            foreground_client_id: None,
            tab_geometry_controllers: HashMap::new(),
            popup_owner_tab_id: None,
            client_shell_boot_id: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            sent_window_title: None,
            api_window_title: None,
            server_keybindings,
            server_config_diagnostic,
            server_config_diagnostic_without_keybindings,
            terminal_attach_owners: HashMap::new(),
            pending_alt_screen_reads: Vec::new(),
            deferred_alt_screen_reads: Vec::new(),
            next_activity_stamp: 1,
            headless_size,
            effective_size: headless_size,
            shutting_down: false,
            handoff_in_progress: false,
            #[cfg(unix)]
            pending_handoff_repaint_nudge: false,
            should_quit,
            server_event_rx,
            server_event_tx,
        })
    }

    /// Runs the headless server event loop until shutdown.
    ///
    /// This is the server's main runtime loop. It:
    /// - Drains internal events (pane death, state changes)
    /// - Drains API requests (from the JSON socket)
    /// - Accepts new client connections
    /// - Reads client messages and routes input
    /// - Handles scheduled tasks (session save, metadata expiry, etc.)
    /// - Renders virtually and streams frames to clients
    pub async fn run(&mut self) -> io::Result<()> {
        crate::logging::startup("server");

        // Register SIGINT handler for graceful shutdown.
        let should_quit = self.should_quit.clone();
        let quit_notify = self.server_event_tx.clone();
        ctrlc_handler(should_quit, quit_notify);

        let mut needs_render = true;
        let mut needs_full_render = true;
        let mut needs_graphics_render = false;

        loop {
            crate::render_prof::event("loop.tick");
            crate::render_prof::flush_if_due();
            self.app.reap_finished_detached_processes();

            // If shutdown has been initiated, complete it and exit.
            if self.shutting_down {
                self.complete_shutdown().await?;
                break;
            }

            // Check if we should start shutting down.
            if self.app.state.should_quit || self.should_quit.load(Ordering::Acquire) {
                self.drain_internal_events_with_forwarding_up_to(
                    crate::app::APP_EVENT_CHANNEL_CAPACITY,
                );
                self.initiate_shutdown();
                continue;
            }

            // 1. Check the coalesced render signal from PTY readers and generic runtime work.
            if self.app.render_dirty.is_pending() {
                needs_render = true;
                crate::render_prof::event("render.request.signal");
            }
            // 2. Drain a bounded internal-event batch. API handlers perform an
            // exhaustive forwarding-aware drain before reading pane/runtime state.
            if self.drain_internal_events_with_forwarding() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
                crate::render_prof::event("full_render_cause.internal_events");
            }
            if self.should_quit.load(Ordering::Acquire) {
                continue;
            }
            if self.app.expire_due_metadata(Instant::now()) {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.metadata_expiry");
            }

            // 3. Drain API requests.
            if self.pane_graphics_runtime_active() {
                let api_impact = self.drain_api_requests_with_render_impact();
                record_render_impact("api_requests", api_impact);
                match api_impact {
                    RenderImpact::None => {}
                    RenderImpact::Graphics => {
                        needs_render = true;
                        needs_graphics_render = true;
                    }
                    RenderImpact::Full => {
                        needs_render = true;
                        needs_full_render = true;
                        needs_graphics_render = false;
                    }
                }
            } else if self.drain_api_requests_with_shutdown_check() {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.api_requests");
            }
            if self.should_quit.load(Ordering::Acquire) {
                continue;
            }

            self.app.sync_focus_events();
            self.app.sync_session_save_schedule();

            // 4. Accept new client connections.
            self.accept_client_connections()?;

            // 5. Drain server events from client threads.
            if self.pane_graphics_runtime_active() {
                let server_impact = self.drain_server_events_with_render_impact();
                record_render_impact("server_events", server_impact);
                match server_impact {
                    RenderImpact::None => {}
                    RenderImpact::Graphics => {
                        needs_render = true;
                        needs_graphics_render = true;
                    }
                    RenderImpact::Full => {
                        needs_render = true;
                        needs_full_render = true;
                        needs_graphics_render = false;
                    }
                }
            } else if self.drain_server_events() {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.server_events");
            }
            if self.should_quit.load(Ordering::Acquire) {
                continue;
            }

            // 6. Handle scheduled tasks.
            let now = Instant::now();
            if self.handle_scheduled_tasks_headless(now, needs_render) {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
                crate::render_prof::event("full_render_cause.scheduled_tasks");
            }

            self.poll_pending_alt_screen_reads(now);
            if self.process_deferred_alt_screen_reads() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
            }

            if latest_shell_client(&self.clients).is_some() && self.app.ensure_default_workspace() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
                crate::render_prof::event("full_render_cause.default_workspace");
            }

            if self.retain_live_pane_graphics() {
                needs_render = true;
                needs_graphics_render = true;
            }
            if self.expire_direct_graphics(now) {
                needs_render = true;
                needs_graphics_render = true;
            }

            self.drain_client_config_reload_request();
            self.sync_immediate_pty_sources();
            self.stream_host_mouse_capture_mode();
            self.stream_direct_terminal_keyboard_mode();

            // 7. Render virtually and stream frames. Hidden-only PTY work keeps a
            // bounded classification cadence without delaying presentation work
            // that joins the same coalesced request.
            let render_cadence_due = self.app.can_render_now(now);
            if needs_render
                && (render_cadence_due
                    || (self.app.can_present_now(now)
                        && self.has_pending_presentation_work(
                            needs_full_render,
                            needs_graphics_render,
                        )))
            {
                crate::render_prof::event("render.attempt");
                let render_request = self.app.render_dirty.take();
                let pty_dirty = !render_request.pty_sources.is_empty();
                if pty_dirty {
                    crate::render_prof::event("render.attempt.pty_dirty");
                    crate::render_prof::counter(
                        "render.attempt.pty_sources",
                        render_request.pty_sources.len() as u64,
                    );
                }
                if render_request.generic {
                    needs_full_render = true;
                    crate::render_prof::event("full_render_cause.generic_dirty");
                }
                let (sidebar_title_changed, outer_title_synced) =
                    self.sync_terminal_title_sources(&render_request.terminal_title_sources);
                if sidebar_title_changed {
                    needs_full_render = true;
                    crate::render_prof::event("full_render_cause.terminal_title_sidebar");
                }
                if needs_full_render && !outer_title_synced {
                    self.sync_window_title();
                }
                if !needs_full_render && !needs_graphics_render && !pty_dirty {
                    // A synchronized-output OSC title can be the only pending work.
                    // Its deferred PTY repaint has its own signal; do not manufacture
                    // a full UI render for this client-local side effect.
                    needs_render = false;
                    continue;
                }
                let hidden_only = pty_dirty
                    && !needs_full_render
                    && !needs_graphics_render
                    && !self.pty_sources_visible_to_any_render_target(&render_request.pty_sources);
                if hidden_only {
                    crate::render_prof::event("render.skipped.hidden_sources");
                } else if !needs_full_render
                    && !needs_graphics_render
                    && self.render_retained_pane_surface_and_stream(&render_request.pty_sources)
                {
                    crate::render_prof::event("retained_surface.invoke");
                } else {
                    crate::render_prof::event("full_render.invoke");
                    self.render_and_stream();
                }
                self.app.record_render_attempt(now, !hidden_only);
                needs_render = false;
                needs_full_render = false;
                needs_graphics_render = false;
                continue;
            }

            // 8. Wait for next event.
            let next_deadline = self
                .app
                .next_headless_loop_deadline_with_git_refresh(
                    now,
                    needs_render,
                    self.has_app_client(),
                )
                .map(|deadline| deadline.min(now + CLIENT_ACCEPT_POLL_INTERVAL))
                .or(Some(now + CLIENT_ACCEPT_POLL_INTERVAL));
            let next_deadline = self
                .pending_alt_screen_reads
                .iter()
                .map(|pending| pending.next_deadline())
                .fold(next_deadline, |deadline, pending| {
                    Some(deadline.map_or(pending, |current| current.min(pending)))
                });
            let event = {
                tokio::select! {
                    maybe_api = self.app.api_rx.recv() => match maybe_api {
                        Some(msg) => LoopEvent::Api(Box::new(msg)),
                        None => LoopEvent::Timer,
                    },
                    maybe_ev = self.app.event_rx.recv() => match maybe_ev {
                        Some(ev) => LoopEvent::Internal(ev),
                        None => LoopEvent::Timer,
                    },
                    maybe_server_ev = self.server_event_rx.recv() => match maybe_server_ev {
                        Some(ev) => LoopEvent::ServerEvent(ev),
                        None => LoopEvent::Timer,
                    },
                    _ = sleep_until_or_pending(next_deadline) => LoopEvent::Timer,
                    _ = self.app.render_notify.notified() => LoopEvent::RenderRequested,
                }
            };

            if self.should_quit.load(Ordering::Acquire) {
                match event {
                    LoopEvent::Internal(ev) => {
                        self.handle_internal_event_with_forwarding(ev);
                    }
                    LoopEvent::ServerEvent(
                        ServerEvent::ClientConnected { writer, .. }
                        | ServerEvent::ClientShellConnected { writer, .. },
                    ) => {
                        if let Ok(message) =
                            Self::frame_server_message(&ServerMessage::ServerShutdown {
                                reason: Some("server is shutting down".to_owned()),
                            })
                        {
                            let _ = writer.control.send(message);
                        }
                    }
                    _ => {}
                }
                continue;
            }

            match event {
                LoopEvent::Timer => {}
                LoopEvent::Internal(ev) => {
                    if self.handle_internal_event_with_forwarding(ev) {
                        needs_render = true;
                        needs_full_render = true;
                        needs_graphics_render = false;
                    }
                }
                LoopEvent::Api(msg) => {
                    if self.pane_graphics_runtime_active() {
                        let impact = self.handle_api_request_with_render_impact(*msg);
                        record_render_impact("api_requests", impact);
                        match impact {
                            RenderImpact::None => {}
                            RenderImpact::Graphics => {
                                needs_render = true;
                                needs_graphics_render = true;
                            }
                            RenderImpact::Full => {
                                needs_render = true;
                                needs_full_render = true;
                                needs_graphics_render = false;
                            }
                        }
                    } else if self.handle_api_request_with_shutdown_check(*msg) {
                        needs_render = true;
                        needs_full_render = true;
                    }
                }
                LoopEvent::ServerEvent(ev) => {
                    if self.pane_graphics_runtime_active() {
                        let impact = self.handle_server_event_with_render_impact(ev);
                        record_render_impact("server_events", impact);
                        match impact {
                            RenderImpact::None => {}
                            RenderImpact::Graphics => {
                                needs_render = true;
                                needs_graphics_render = true;
                            }
                            RenderImpact::Full => {
                                needs_render = true;
                                needs_full_render = true;
                                needs_graphics_render = false;
                            }
                        }
                    } else if self.handle_server_event(ev) {
                        needs_render = true;
                        needs_full_render = true;
                    }
                }
                LoopEvent::RenderRequested => {
                    if self.app.render_dirty.is_pending() {
                        needs_render = true;
                    }
                }
            }
        }

        // Save session on exit.
        if self.app.policy.persist_session {
            self.app.save_session_now();
        }

        info!("headless server exiting");
        Ok(())
    }

    fn allocate_activity_stamp(&mut self) -> u64 {
        let stamp = self.next_activity_stamp;
        self.next_activity_stamp = self.next_activity_stamp.saturating_add(1);
        stamp
    }

    #[cfg(unix)]
    fn resize_shared_runtime_to_effective_size(&mut self) {
        self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(true);
    }

    fn resize_shared_runtime_to_effective_size_before_input(&mut self) {
        self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(false);
    }

    fn resize_shared_runtime_to_effective_size_with_pending_agent_resumes(
        &mut self,
        start_pending_agent_resumes: bool,
    ) {
        let Some(client_id) = self.foreground_client_id else {
            return;
        };
        let Some(client) = self.clients.get(&client_id) else {
            return;
        };
        if matches!(client.mode, ClientConnectionMode::ClientShell) {
            self.resize_shell_tab_if_controller(client_id, start_pending_agent_resumes);
            return;
        }
        let cell_size = client.cell_size;
        let (cols, rows) = self.effective_size;
        let area = Rect::new(0, 0, cols, rows);
        if self.app.state.kitty_graphics_enabled && cell_size.is_known() {
            crate::ui::compute_view_with_cell_size(
                &mut self.app.state,
                &self.app.terminal_runtimes,
                area,
                cell_size,
            );
        } else {
            crate::ui::compute_view_with_runtime_registry(
                &mut self.app.state,
                &self.app.terminal_runtimes,
                area,
            );
        }

        // Shared runtime size changes affect pane wrapping and foreground-driven
        // rendering semantics. Force one fresh frame to every remaining client
        // even if the next rendered buffer compares equal to its cached frame.
        for client in self.clients.values_mut() {
            client.request_repaint();
        }
        if !start_pending_agent_resumes {
            self.app.pending_agent_resume_deadline = None;
            return;
        }
        let now = Instant::now();
        self.app.sync_pending_agent_resume_deadline(now);
        if self
            .app
            .start_pending_agent_resumes(self.app.pending_agent_resume_due(now))
        {
            for client in self.clients.values_mut() {
                client.request_repaint();
            }
        }
    }

    fn sync_runtime_view_geometry(&mut self) {
        crate::ui::compute_view_without_resizing_panes(
            &mut self.app.state,
            &self.app.terminal_runtimes,
            Rect::new(0, 0, self.effective_size.0, self.effective_size.1),
        );
    }

    fn sync_foreground_client_state(&mut self) {
        self.app.direct_graphics_available = self.direct_graphics_available();
        self.app.pixel_mouse_available = self.foreground_client_id.is_some_and(|id| {
            self.clients
                .get(&id)
                .is_some_and(|client| client.pixel_mouse)
        });
        if !self.app.direct_graphics_available {
            self.retire_all_direct_graphics();
        }
        let Some(client_id) = self.foreground_client_id else {
            self.effective_size = self.headless_size;
            self.app.state.outer_terminal_focus = None;
            self.app.state.host_cell_size = crate::kitty_graphics::HostCellSize::default();
            self.sync_runtime_view_geometry();
            let server_keybindings = self.server_keybindings.clone();
            apply_keybindings(&mut self.app, &server_keybindings);
            self.sync_visible_server_config_diagnostic(false);
            return;
        };
        let Some(client) = self.clients.get(&client_id) else {
            self.foreground_client_id = None;
            self.effective_size = self.headless_size;
            self.app.state.outer_terminal_focus = None;
            self.app.state.host_cell_size = crate::kitty_graphics::HostCellSize::default();
            self.sync_runtime_view_geometry();
            let server_keybindings = self.server_keybindings.clone();
            apply_keybindings(&mut self.app, &server_keybindings);
            self.sync_visible_server_config_diagnostic(false);
            return;
        };

        let terminal_size = client.terminal_size;
        let host_cell_size = if self.app.state.kitty_graphics_enabled && client.cell_size.is_known()
        {
            client.cell_size
        } else {
            crate::kitty_graphics::HostCellSize::default()
        };
        let host_terminal_theme = client.host_terminal_theme;
        let host_terminal_appearance = client.host_terminal_appearance;
        let host_terminal_appearance_explicit = client.host_terminal_appearance_explicit;
        let outer_terminal_focus = client.outer_terminal_focus;

        self.effective_size = terminal_size;
        self.sync_runtime_view_geometry();
        self.app.state.outer_terminal_focus = outer_terminal_focus;
        self.app.state.host_cell_size = host_cell_size;
        let server_keybindings = self.server_keybindings.clone();
        apply_keybindings(&mut self.app, &server_keybindings);
        self.sync_visible_server_config_diagnostic(false);
        if outer_terminal_focus == Some(true) {
            self.app.state.mark_active_tab_seen();
        }
        self.app.set_host_terminal_appearance_state(
            host_terminal_appearance,
            host_terminal_appearance_explicit,
        );
        self.app.set_host_terminal_theme(host_terminal_theme);
    }

    fn sync_visible_server_config_diagnostic(&mut self, uses_local_keybindings: bool) {
        let visible = if uses_local_keybindings {
            &self.server_config_diagnostic_without_keybindings
        } else {
            &self.server_config_diagnostic
        };
        if self.app.state.config_diagnostic == self.server_config_diagnostic
            || self.app.state.config_diagnostic == self.server_config_diagnostic_without_keybindings
        {
            self.app.state.config_diagnostic = visible.clone();
        }
    }

    fn reload_server_config(&mut self, notify_success: bool) -> crate::config::ConfigReloadReport {
        let server_keybindings = self.server_keybindings.clone();
        apply_keybindings(&mut self.app, &server_keybindings);
        let report = self.app.apply_config_from_disk(notify_success);
        self.app.take_config_reloaded_from_disk();
        self.server_keybindings = app_keybindings(&self.app);
        self.headless_size = self.app.state.headless_size;
        let (server_config_diagnostic, server_config_diagnostic_without_keybindings) =
            server_config_diagnostic_summaries(&report.diagnostics);
        self.server_config_diagnostic = server_config_diagnostic;
        self.server_config_diagnostic_without_keybindings =
            server_config_diagnostic_without_keybindings;
        self.sync_foreground_client_state();
        report
    }

    fn foreground_client_outer_focus(&self) -> Option<bool> {
        let client_id = self.foreground_client_id?;
        self.clients.get(&client_id)?.outer_terminal_focus
    }

    fn active_tab_suppresses_notifications(&self, is_active_tab: bool) -> bool {
        crate::app::actions::active_tab_suppresses_notifications(
            is_active_tab,
            self.foreground_client_outer_focus(),
        )
    }

    fn promote_client_to_foreground(&mut self, client_id: u64) -> bool {
        let stamp = self.allocate_activity_stamp();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        client.last_activity = stamp;

        let changed = self.foreground_client_id != Some(client_id);
        self.foreground_client_id = Some(client_id);
        self.sync_foreground_client_state();
        changed
    }

    fn promote_latest_remaining_client(&mut self) -> bool {
        let next_foreground = latest_shell_client(&self.clients);
        let changed = next_foreground != self.foreground_client_id;
        self.foreground_client_id = next_foreground;
        self.sync_foreground_client_state();
        changed
    }

    fn app_client_count(&self) -> usize {
        self.clients
            .values()
            .filter(|client| client.is_shell_client() && client.writer.is_some())
            .count()
    }

    fn client_supports_direct_graphics(&self, client_id: u64) -> bool {
        self.clients.get(&client_id).is_some_and(|client| {
            client.is_shell_client()
                && client.writer.is_some()
                && client.direct_graphics
                && client.pixel_mouse
        })
    }

    fn existing_direct_graphics_client(&self) -> Option<u64> {
        self.app
            .pane_graphics
            .slots
            .values()
            .filter_map(crate::app::pane_graphics::Slot::direct_client)
            .filter_map(|client_id| {
                let client = self.clients.get(&client_id)?;
                self.client_supports_direct_graphics(client_id)
                    .then_some((client.last_activity, client_id))
            })
            .max()
            .map(|(_, client_id)| client_id)
    }

    fn direct_graphics_client(&self) -> Option<u64> {
        self.existing_direct_graphics_client().or_else(|| {
            self.foreground_client_id
                .filter(|client_id| self.client_supports_direct_graphics(*client_id))
        })
    }

    fn direct_graphics_client_for_key(&self, key: &crate::app::pane_graphics::Key) -> Option<u64> {
        match self
            .app
            .pane_graphics
            .slots
            .get(key)
            .and_then(crate::app::pane_graphics::Slot::direct_client)
        {
            Some(client_id) => self
                .client_supports_direct_graphics(client_id)
                .then_some(client_id),
            None => self.direct_graphics_client(),
        }
    }

    fn direct_graphics_available(&self) -> bool {
        self.direct_graphics_client().is_some()
    }

    fn has_app_client(&self) -> bool {
        self.app_client_count() > 0
    }

    fn remove_client(&mut self, client_id: u64) -> bool {
        self.retire_direct_graphics_for_client(client_id);
        let disconnected_focus = self
            .clients
            .get(&client_id)
            .filter(|client| client.is_shell_client() && client.outer_terminal_focus == Some(true))
            .and_then(|_| self.shell_focus_target(client_id));
        let should_release_focus = disconnected_focus.as_ref().is_some_and(|target| {
            !self.clients.iter().any(|(&other_id, client)| {
                other_id != client_id
                    && client.is_shell_client()
                    && client.outer_terminal_focus == Some(true)
                    && self.shell_tab_id_for_client(other_id).as_deref()
                        == Some(target.tab_id.as_str())
            })
        });
        let was_foreground = self.foreground_client_id == Some(client_id);
        let removed = self.clients.remove(&client_id);
        self.tab_geometry_controllers
            .retain(|_, controller_id| *controller_id != client_id);
        if let Some(mut removed) = removed {
            self.release_client_shell_inputs(client_id, &mut removed);
            crate::server::clipboard_image::remove_files(removed.staged_clipboard_files);
            if let ClientConnectionMode::TerminalAttach { terminal_id } = removed.mode {
                self.terminal_attach_owners.remove(&terminal_id);
                if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
                    self.app
                        .state
                        .direct_attach_resize_locks
                        .remove(&terminal_id);
                }
            }
        }
        if should_release_focus {
            if let Some(target) = disconnected_focus.as_ref() {
                self.send_shell_focus_target(target, crate::ghostty::FocusEvent::Lost);
            }
        }
        self.app.direct_graphics_available = self.direct_graphics_available();
        if was_foreground {
            self.promote_latest_remaining_client()
        } else {
            false
        }
    }

    fn release_client_shell_inputs(&mut self, client_id: u64, client: &mut ClientConnection) {
        for held in client.drain_shell_held_inputs() {
            let result = match held.target {
                ClientShellInputTarget::Pane(pane_id) => {
                    let Some((workspace_index, runtime_pane_id)) = self.app.parse_pane_id(&pane_id)
                    else {
                        continue;
                    };
                    let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                        &self.app.terminal_runtimes,
                        workspace_index,
                        runtime_pane_id,
                    ) else {
                        continue;
                    };
                    apply_client_pane_input_events(runtime, &[held.release])
                }
                ClientShellInputTarget::Popup(terminal_id) => {
                    let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) else {
                        continue;
                    };
                    apply_client_popup_input_events(runtime, &[held.release])
                }
            };
            if let Err(err) = result {
                warn!(client_id, err = %err, "client shell teardown release failed");
            }
        }
    }

    fn remove_client_and_resize_if_needed(&mut self, client_id: u64) {
        let restore_shell_controller = self.clients.get(&client_id).and_then(|client| {
            let ClientConnectionMode::TerminalAttach { terminal_id } = &client.mode else {
                return None;
            };
            self.shell_geometry_controller_for_terminal(terminal_id)
        });
        self.remove_client(client_id);
        if let Some((controller_id, target)) = restore_shell_controller {
            self.restore_shell_tab_geometry(controller_id, target);
        } else {
            self.resize_tabs_for_only_shell_client(true);
        }
    }

    /// Accepts pending client connections from the non-blocking listener.
    #[cfg(unix)]
    fn accept_client_connections(&mut self) -> io::Result<()> {
        if self.handoff_in_progress {
            return reject_pending_client_connections(&self.client_listener);
        }
        accept_pending_client_connections(
            &self.client_listener,
            &mut self.next_client_id,
            &self.should_quit,
            &self.server_event_tx,
        )
    }

    /// Windows named-pipe clients can block in connect unless the server has a
    /// pending blocking accept. The dedicated accept thread handles that path.
    #[cfg(windows)]
    fn accept_client_connections(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Drains server events from the dedicated channel.
    ///
    /// Uses the original full-render semantics when pane graphics are dormant.
    fn drain_server_events(&mut self) -> bool {
        let mut changed = false;
        while !self.should_quit.load(Ordering::Acquire) {
            let Ok(ev) = self.server_event_rx.try_recv() else {
                break;
            };
            changed |= self.handle_server_event(ev);
        }
        changed
    }

    /// Returns the strongest render impact from the drained event batch.
    fn drain_server_events_with_render_impact(&mut self) -> RenderImpact {
        let mut impact = RenderImpact::None;
        while !self.should_quit.load(Ordering::Acquire) {
            let Ok(ev) = self.server_event_rx.try_recv() else {
                break;
            };
            impact.merge(self.handle_server_event_with_render_impact(ev));
        }
        impact
    }

    async fn reject_late_client_connections(&mut self) {
        self.server_event_rx.close();
        while let Some(event) = self.server_event_rx.recv().await {
            if let ServerEvent::ClientConnected { writer, .. }
            | ServerEvent::ClientShellConnected { writer, .. } = event
            {
                if let Ok(message) = Self::frame_server_message(&ServerMessage::ServerShutdown {
                    reason: Some("server is shutting down".to_owned()),
                }) {
                    let _ = writer.control.send(message);
                }
            }
        }
    }

    fn terminal_id_by_string(&self, terminal_id: &str) -> Option<crate::terminal::TerminalId> {
        self.app
            .state
            .terminals
            .keys()
            .find(|id| id.to_string() == terminal_id)
            .cloned()
    }

    fn runtime_for_terminal_id_string(
        &self,
        terminal_id: &str,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        let terminal_id = self.terminal_id_by_string(terminal_id)?;
        self.app.terminal_runtimes.get(&terminal_id)
    }

    fn resolve_terminal_target_id_string(&self, target: &str) -> Option<String> {
        if self.terminal_id_by_string(target).is_some() {
            return Some(target.to_owned());
        }
        self.app
            .resolve_terminal_target(target)
            .ok()
            .map(|resolved| resolved.terminal_id)
    }

    fn client_clipboard_image_target_is_valid(
        &self,
        client_id: u64,
        target: &protocol::ClientClipboardImageTarget,
    ) -> bool {
        match target {
            protocol::ClientClipboardImageTarget::DirectTerminal => {
                self.clients.get(&client_id).is_some_and(|client| {
                    matches!(client.mode, ClientConnectionMode::TerminalAttach { .. })
                })
            }
            protocol::ClientClipboardImageTarget::Pane(pane_id) => {
                !self.handoff_in_progress
                    && self.app.state.popup_pane.is_none()
                    && self.clients.get(&client_id).is_some_and(|client| {
                        matches!(client.mode, ClientConnectionMode::ClientShell)
                    })
                    && self.app.parse_pane_id(pane_id).is_some()
            }
            protocol::ClientClipboardImageTarget::Popup(terminal_id) => {
                !self.handoff_in_progress
                    && self.clients.get(&client_id).is_some_and(|client| {
                        matches!(client.mode, ClientConnectionMode::ClientShell)
                    })
                    && self
                        .app
                        .state
                        .popup_pane
                        .as_ref()
                        .is_some_and(|popup| popup.terminal_id.as_str() == terminal_id)
            }
        }
    }

    fn stage_client_clipboard_image(
        &self,
        client_id: u64,
        extension: &str,
        data: &[u8],
    ) -> std::io::Result<crate::server::clipboard_image::StagedClipboardImage> {
        let staged = crate::server::clipboard_image::stage(client_id, extension, data)?;
        info!(client_id, bytes = data.len(), path = %staged.paste_text, "staged client clipboard image");
        Ok(staged)
    }

    fn paste_client_clipboard_image_path(
        &mut self,
        client_id: u64,
        target: protocol::ClientClipboardImageTarget,
        path: String,
    ) -> bool {
        match target {
            protocol::ClientClipboardImageTarget::DirectTerminal => {
                let Some(ClientConnection {
                    mode: ClientConnectionMode::TerminalAttach { terminal_id },
                    ..
                }) = self.clients.get(&client_id)
                else {
                    return false;
                };
                if let Some(runtime) = self.runtime_for_terminal_id_string(terminal_id) {
                    let payload = paste_payload_for_runtime(runtime, &path);
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(payload)) {
                        warn!(client_id, terminal_id = %terminal_id, err = %err, "terminal attach clipboard image paste failed");
                    }
                }
                true
            }
            protocol::ClientClipboardImageTarget::Pane(pane_id) => {
                if self.handoff_in_progress
                    || !self.clients.get(&client_id).is_some_and(|client| {
                        matches!(client.mode, ClientConnectionMode::ClientShell)
                    })
                {
                    return false;
                }
                let Some((workspace_index, runtime_pane_id)) = self.app.parse_pane_id(&pane_id)
                else {
                    return false;
                };
                let popup_blocks_input = self.app.state.popup_pane.is_some()
                    && self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id);
                if popup_blocks_input
                    || !self.shell_client_views_pane(client_id, workspace_index, runtime_pane_id)
                {
                    return false;
                }
                let foreground_changed = self.promote_client_to_foreground(client_id);
                let geometry_changed = self.claim_shell_tab_geometry(client_id, false);
                let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                    &self.app.terminal_runtimes,
                    workspace_index,
                    runtime_pane_id,
                ) else {
                    return foreground_changed | geometry_changed;
                };
                if let Err(err) = apply_client_pane_input_events(
                    runtime,
                    &[protocol::ClientPaneInputEvent::Paste(path)],
                ) {
                    warn!(client_id, pane_id, err = %err, "client shell clipboard image paste failed");
                }
                true
            }
            protocol::ClientClipboardImageTarget::Popup(terminal_id) => {
                if self.handoff_in_progress
                    || !self.clients.get(&client_id).is_some_and(|client| {
                        matches!(client.mode, ClientConnectionMode::ClientShell)
                    })
                {
                    return false;
                }
                let Some(popup_terminal_id) = self
                    .app
                    .state
                    .popup_pane
                    .as_ref()
                    .map(|popup| popup.terminal_id.clone())
                else {
                    return false;
                };
                if popup_terminal_id.as_str() != terminal_id
                    || self.popup_owner_tab_id != self.shell_tab_id_for_client(client_id)
                {
                    return false;
                }
                let foreground_changed = self.promote_client_to_foreground(client_id);
                let geometry_changed = self.claim_shell_tab_geometry(client_id, false);
                let Some(runtime) = self.app.terminal_runtimes.get(&popup_terminal_id) else {
                    return foreground_changed | geometry_changed;
                };
                if let Err(err) = apply_client_popup_input_events(
                    runtime,
                    &[protocol::ClientPaneInputEvent::Paste(path)],
                ) {
                    warn!(client_id, terminal_id, err = %err, "client shell popup clipboard image paste failed");
                }
                true
            }
        }
    }

    fn resolve_terminal_session_target(
        &mut self,
        client_id: u64,
        target: &str,
        action: &str,
    ) -> Option<String> {
        if !self.client_is_pending_terminal_mode(client_id) {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(
                        format!(
                            "terminal session {action} failed: connection is not pending terminal session"
                        ),
                    ),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return None;
        }

        let Some(terminal_id) = self.resolve_terminal_target_id_string(target) else {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(format!(
                        "terminal session {action} failed: terminal target {target} not found"
                    )),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return None;
        };

        Some(terminal_id)
    }

    fn observe_terminal_client(&mut self, client_id: u64, target: String) -> bool {
        let Some(terminal_id) = self.resolve_terminal_session_target(client_id, &target, "observe")
        else {
            return false;
        };

        let stamp = self.allocate_activity_stamp();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let (cols, rows) = client.terminal_size;
        client.mode = ClientConnectionMode::TerminalObserve {
            terminal_id: terminal_id.clone(),
        };
        client.render_state.reset_baseline();
        client.last_activity = stamp;
        let was_foreground = self.foreground_client_id == Some(client_id);
        if was_foreground {
            self.promote_latest_remaining_client();
        }

        info!(client_id, cols, rows, terminal_id = %terminal_id, "terminal observe client connected");
        true
    }

    fn control_terminal_client(&mut self, client_id: u64, target: String, takeover: bool) -> bool {
        let Some(terminal_id) = self.resolve_terminal_session_target(client_id, &target, "control")
        else {
            return false;
        };

        self.attach_terminal_client(client_id, terminal_id, takeover)
    }

    fn handle_terminal_attach_scroll(
        &mut self,
        client_id: u64,
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    ) -> bool {
        let Some(ClientConnection {
            mode: ClientConnectionMode::TerminalAttach { terminal_id },
            ..
        }) = self.clients.get(&client_id)
        else {
            return false;
        };
        let Some(runtime) = self.runtime_for_terminal_id_string(terminal_id) else {
            return false;
        };

        if let Err(err) =
            apply_terminal_attach_scroll(runtime, source, direction, lines, column, row, modifiers)
        {
            warn!(client_id, terminal_id = %terminal_id, err = %err, "terminal attach scroll failed");
        }
        true
    }

    fn handle_terminal_attach_mouse(
        &mut self,
        client_id: u64,
        kind: protocol::ClientMouseKind,
        position: protocol::ClientMousePosition,
        geometry: Option<protocol::ClientMouseGeometry>,
        modifiers: u8,
        lines: u16,
    ) -> bool {
        if self.handoff_in_progress {
            return false;
        }
        let Some(client) = self.clients.get(&client_id) else {
            return false;
        };
        let ClientConnectionMode::TerminalAttach { terminal_id } = &client.mode else {
            return false;
        };
        let terminal_id = terminal_id.clone();
        let terminal_size = client.terminal_size;
        let cell_size = client.cell_size;
        let pixel_mouse = client.pixel_mouse;
        let host_sgr_pixels_active = client.host_sgr_pixels_active == Some(true);
        let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) else {
            return false;
        };
        let Some(position) = terminal_attach_mouse_position(
            runtime,
            terminal_size,
            cell_size,
            pixel_mouse,
            host_sgr_pixels_active,
            position,
            geometry,
        ) else {
            return false;
        };
        let event = protocol::ClientPaneInputEvent::Mouse {
            kind,
            position,
            geometry: None,
            modifiers,
            lines: lines.max(1),
        };
        if let Err(err) = apply_client_pane_input_events(runtime, &[event]) {
            warn!(client_id, terminal_id = %terminal_id, err = %err, "terminal attach mouse input failed");
        }
        true
    }

    /// Pulls only titles reported dirty by the PTY parser. A focused pane title
    /// is forwarded as an independent client side effect; only sidebar title
    /// tokens require a UI render.
    fn sync_terminal_title_sources(
        &mut self,
        sources: &HashSet<crate::layout::PaneId>,
    ) -> (bool, bool) {
        let focused_source = self
            .app
            .state
            .active
            .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
            .and_then(|workspace| workspace.focused_pane_id())
            .is_some_and(|pane_id| sources.contains(&pane_id));
        let changes = self.app.sync_terminal_titles(sources);
        let outer_title_synced = focused_source && self.app.window_title_uses_terminal_title();
        if outer_title_synced {
            self.sync_window_title();
        }
        (
            self.app.terminal_title_sidebar_changed(&changes),
            outer_title_synced,
        )
    }

    /// Renders `ui.window_title` against current session state. `None` means
    /// window titles are disabled or every token resolved empty, which leaves
    /// the client on Herdr's default title.
    fn configured_window_title(&self) -> Option<String> {
        self.app
            .window_title()
            .and_then(|title| crate::config::sanitize_window_title_text(&title))
    }

    /// Pushes the configured outer window title to the foreground client when it
    /// changed. Herdr consumes each pane's own `OSC 0`/`OSC 2`, so without this
    /// the host terminal title never follows the session — which is what window
    /// managers read for tab and group bar labels.
    fn sync_window_title(&mut self) {
        let title = match &self.api_window_title {
            Some(title) => Some(title.clone()),
            None if self.app.window_title_configured() => self.configured_window_title(),
            None => return,
        };
        if let (Some(client_id), Some((sent_client_id, sent_title))) =
            (self.foreground_client_id, self.sent_window_title.as_ref())
        {
            if *sent_client_id == client_id && *sent_title == title {
                return;
            }
        }
        self.send_window_title(title);
    }

    /// Sends a window title and remembers it only when a foreground client took
    /// it, so the next client to attach is written to rather than skipped.
    fn send_window_title(&mut self, title: Option<String>) -> bool {
        let Some(client_id) = self.foreground_client_id else {
            self.sent_window_title = None;
            return false;
        };
        // A detached client keeps its entry with no writer, and a targeted send
        // to one reports success without queuing anything. Caching the title
        // against that client would skip the send once it attaches again.
        if self
            .clients
            .get(&client_id)
            .is_none_or(|client| client.writer.is_none())
        {
            self.sent_window_title = None;
            return false;
        }
        let sent = self.send_to_client(
            client_id,
            ServerMessage::WindowTitle {
                title: title.clone(),
            },
        );
        self.sent_window_title = sent.then_some((client_id, title));
        sent
    }

    fn handle_client_window_title_api(&mut self, id: String, title: Option<String>) -> String {
        use api::schema::{ClientWindowTitleReason, ResponseResult};

        let title = match title {
            Some(title) => match crate::config::sanitize_window_title_text(&title) {
                Some(title) => Some(title),
                None => {
                    return serde_json::to_string(&api::schema::ErrorResponse {
                        id,
                        error: api::schema::ErrorBody {
                            code: "invalid_params".into(),
                            message: "window title is empty".into(),
                        },
                    })
                    .unwrap_or_else(|_| "{}".to_string());
                }
            },
            None => None,
        };
        let set_title = title.is_some();
        // An explicit title suppresses `ui.window_title` until it is cleared,
        // and clearing restores the configured title rather than only "herdr".
        self.api_window_title = title.clone();
        let title = title.or_else(|| self.configured_window_title());
        let changed = self.send_window_title(title);
        let reason = match (changed, set_title) {
            (true, true) => ClientWindowTitleReason::Set,
            (true, false) => ClientWindowTitleReason::Cleared,
            (false, _) => ClientWindowTitleReason::NoForegroundClient,
        };
        serde_json::to_string(&api::schema::SuccessResponse {
            id,
            result: ResponseResult::ClientWindowTitle { changed, reason },
        })
        .unwrap_or_else(|_| "{}".to_string())
    }

    fn drain_client_config_reload_request(&mut self) {
        if !self.app.state.request_client_config_reload {
            return;
        }
        self.app.state.request_client_config_reload = false;
        self.send_to_all_clients(ServerMessage::ReloadSoundConfig);
    }

    /// Encodes a server message into a length-prefixed frame.
    fn frame_server_message(msg: &ServerMessage) -> Result<Vec<u8>, protocol::FramingError> {
        Self::frame_server_message_with_max(msg, MAX_FRAME_SIZE)
    }

    /// Encodes a server message using an explicit payload cap.
    fn frame_server_message_with_max(
        msg: &ServerMessage,
        max_frame_size: usize,
    ) -> Result<Vec<u8>, protocol::FramingError> {
        let mut framed = Vec::new();
        protocol::write_message(&mut framed, msg)?;
        let payload_len = framed.len().saturating_sub(4);
        if payload_len > max_frame_size {
            return Err(protocol::FramingError::Oversized {
                claimed: payload_len,
                max: max_frame_size,
            });
        }
        Ok(framed)
    }

    /// Sends a message to all connected clients.
    /// Broken connections are tracked and cleaned up.
    fn send_to_all_clients(&mut self, msg: ServerMessage) {
        let serialized = match Self::frame_server_message(&msg) {
            Ok(framed) => framed,
            Err(err) => {
                warn!(err = %err, "failed to serialize message for clients");
                return;
            }
        };

        let mut broken_clients: Vec<u64> = Vec::new();
        for (&client_id, client) in &mut self.clients {
            if let Some(writer) = &client.writer {
                if writer.control.send(serialized.clone()).is_err() {
                    debug!(client_id, "client writer channel closed during broadcast");
                    broken_clients.push(client_id);
                }
            }
        }

        // Remove broken clients.
        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    /// Sends an ephemeral semantic event to every connected client-rendered shell.
    fn send_to_client_shells(&mut self, msg: ServerMessage) -> bool {
        let serialized = match Self::frame_server_message(&msg) {
            Ok(framed) => framed,
            Err(err) => {
                warn!(err = %err, "failed to serialize message for client shells");
                return false;
            }
        };
        let client_ids = self
            .clients
            .iter()
            .filter_map(|(&client_id, client)| {
                matches!(client.mode, ClientConnectionMode::ClientShell).then_some(client_id)
            })
            .collect::<Vec<_>>();
        let mut sent = false;
        for client_id in client_ids {
            let Some(client) = self.clients.get(&client_id) else {
                continue;
            };
            let Some(writer) = &client.writer else {
                continue;
            };
            if writer.control.send(serialized.clone()).is_ok() {
                sent = true;
            } else {
                self.remove_client_and_resize_if_needed(client_id);
            }
        }
        sent
    }

    /// Sends a client-local side effect to the foreground client only.
    fn send_to_foreground_client(&mut self, msg: ServerMessage) -> bool {
        let Some(client_id) = self.foreground_client_id else {
            return false;
        };
        self.send_to_client(client_id, msg)
    }

    /// Sends a message to a specific client. Returns false if the client
    /// was not found or the send failed (client removed).
    fn send_to_client(&mut self, client_id: u64, msg: ServerMessage) -> bool {
        let serialized = match Self::frame_server_message(&msg) {
            Ok(framed) => framed,
            Err(err) => {
                warn!(client_id, err = %err, "failed to serialize message for client");
                return false;
            }
        };

        if let Some(client) = self.clients.get(&client_id) {
            if let Some(writer) = &client.writer {
                if writer.control.send(serialized).is_err() {
                    debug!(
                        client_id,
                        "client writer channel closed during targeted send"
                    );
                    self.remove_client_and_resize_if_needed(client_id);
                    return false;
                }
            }
            true
        } else {
            false
        }
    }

    fn shutdown_terminal_stream_clients(&mut self, terminal_id: &str, reason: String) {
        let client_ids = terminal_stream_client_ids(&self.clients, terminal_id);

        for client_id in client_ids {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(reason.clone()),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    fn send_terminal_stream_detach_shutdown(&mut self, client_id: u64) {
        if matches!(
            self.clients.get(&client_id).map(|client| &client.mode),
            Some(
                ClientConnectionMode::TerminalAttach { .. }
                    | ClientConnectionMode::TerminalObserve { .. }
            )
        ) {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some("detached".to_owned()),
                },
            );
        }
    }

    #[cfg(unix)]
    fn disconnect_all_clients_for_handoff(&mut self) {
        let client_ids = self.clients.keys().copied().collect::<Vec<_>>();
        for client_id in client_ids {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(
                        "live update in progress; reconnect after handoff completes".to_owned(),
                    ),
                },
            );
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.writer = None;
            }
            let _ = self.remove_client(client_id);
        }
        self.foreground_client_id = None;
        self.sync_foreground_client_state();
        self.resize_shared_runtime_to_effective_size();
    }

    fn attach_terminal_client(
        &mut self,
        client_id: u64,
        terminal_id: String,
        takeover: bool,
    ) -> bool {
        if !self.client_is_pending_terminal_mode(client_id) {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(
                        "terminal attach failed: connection is not pending terminal attach"
                            .to_owned(),
                    ),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return false;
        }

        let Some(real_terminal_id) = self.terminal_id_by_string(&terminal_id) else {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(format!(
                        "terminal attach failed: terminal {terminal_id} not found"
                    )),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return false;
        };

        if self
            .pending_alt_screen_reads
            .iter()
            .any(|pending| pending.terminal_id == real_terminal_id)
        {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(format!(
                        "terminal attach failed: terminal {terminal_id} has a read in progress; retry"
                    )),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return false;
        }

        if let Some(existing_owner) = self.terminal_attach_owners.get(&terminal_id).copied() {
            if existing_owner != client_id && !takeover {
                self.send_to_client(
                    client_id,
                    ServerMessage::ServerShutdown {
                        reason: Some(format!(
                            "terminal attach failed: terminal {terminal_id} already has an attached client; retry with --takeover"
                        )),
                    },
                );
                self.remove_client_and_resize_if_needed(client_id);
                return false;
            }
            if existing_owner != client_id {
                self.send_to_client(
                    existing_owner,
                    ServerMessage::ServerShutdown {
                        reason: Some("terminal attach taken over".to_owned()),
                    },
                );
                self.remove_client_and_resize_if_needed(existing_owner);
            }
        }

        let stamp = self.allocate_activity_stamp();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let (cols, rows) = client.terminal_size;
        let cell_size = client.cell_size;
        client.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: terminal_id.clone(),
        };
        client.render_state.reset_baseline();
        client.last_activity = stamp;
        let was_foreground = self.foreground_client_id == Some(client_id);
        if was_foreground {
            self.promote_latest_remaining_client();
        }

        info!(client_id, cols, rows, terminal_id = %terminal_id, "terminal attach client connected");
        self.terminal_attach_owners
            .insert(terminal_id.clone(), client_id);
        self.app
            .state
            .direct_attach_resize_locks
            .insert(real_terminal_id.clone());
        self.app
            .start_pending_agent_resume_for_terminal(&real_terminal_id, rows, cols, true);
        if let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) {
            runtime.resize(rows, cols, cell_size.width_px, cell_size.height_px);
        }
        true
    }

    fn client_is_pending_terminal_mode(&self, client_id: u64) -> bool {
        self.clients
            .get(&client_id)
            .is_some_and(|client| matches!(client.mode, ClientConnectionMode::TerminalPending))
    }

    /// Handles a server event. Returns true if the event requires a re-render.
    fn handle_server_event(&mut self, ev: ServerEvent) -> bool {
        if self.handoff_in_progress && Self::ignore_client_event_during_handoff(&ev) {
            return false;
        }

        match ev {
            ServerEvent::ClientConnected {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
                writer,
            } => {
                if self.handoff_in_progress {
                    if let Ok(message) =
                        Self::frame_server_message(&ServerMessage::ServerShutdown {
                            reason: Some(
                                "live update in progress; reconnect after handoff completes"
                                    .to_owned(),
                            ),
                        })
                    {
                        let _ = writer.control.send(message);
                    }
                    return false;
                }
                info!(
                    client_id,
                    cols, rows, cell_width_px, cell_height_px, "direct terminal client connected"
                );
                let last_activity = self.allocate_activity_stamp();
                let observed = crate::kitty_graphics::HostCellSize {
                    width_px: cell_width_px,
                    height_px: cell_height_px,
                };
                let pixel_mouse = pixel_mouse && observed.is_known();
                let mut connection = ClientConnection::new_with_mode(
                    ClientConnectionMode::TerminalPending,
                    (cols, rows),
                    observed,
                    last_activity,
                    protocol::RenderEncoding::TerminalAnsi,
                    Some(writer),
                );
                connection.pixel_mouse = pixel_mouse;
                self.clients.insert(client_id, connection);
                false
            }
            ServerEvent::ClientShellConnected {
                client_id,
                surface_cols,
                surface_rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
                direct_graphics,
                endpoint_keybindings,
                mouse_capture,
                writer,
            } => {
                if self.handoff_in_progress {
                    if let Ok(message) =
                        Self::frame_server_message(&ServerMessage::ServerShutdown {
                            reason: Some(
                                "live update in progress; reconnect after handoff completes"
                                    .to_owned(),
                            ),
                        })
                    {
                        let _ = writer.control.send(message);
                    }
                    return false;
                }
                info!(
                    client_id,
                    cols = surface_cols,
                    rows = surface_rows,
                    cell_width_px,
                    cell_height_px,
                    render_encoding = ?protocol::RenderEncoding::SemanticFrame,
                    "client connected"
                );
                self.app.ensure_default_workspace();
                let first_app_client = self.app_client_count() == 0;
                let last_activity = self.allocate_activity_stamp();
                let observed = crate::kitty_graphics::HostCellSize {
                    width_px: cell_width_px,
                    height_px: cell_height_px,
                };
                let mut connection = ClientConnection::new_with_mode(
                    ClientConnectionMode::ClientShell,
                    (surface_cols, surface_rows),
                    observed,
                    last_activity,
                    protocol::RenderEncoding::SemanticFrame,
                    Some(writer),
                );
                connection.pixel_mouse = pixel_mouse && observed.is_known();
                connection.direct_graphics = direct_graphics;
                connection.shell_uses_endpoint_keybindings = endpoint_keybindings;
                connection.shell_mouse_capture = mouse_capture;
                connection.shell_projection_revision = 1;
                let config_diagnostic = if endpoint_keybindings {
                    self.server_config_diagnostic.as_deref()
                } else {
                    self.server_config_diagnostic_without_keybindings.as_deref()
                };
                let seed_snapshot = client_shell_snapshot(
                    &self.app,
                    &self.client_shell_boot_id,
                    connection.shell_projection_revision,
                    config_diagnostic,
                    None,
                );
                let location =
                    crate::server::clients::ClientShellLocation::from_snapshot(&seed_snapshot);
                let snapshot_message =
                    match crate::protocol::endpoint::snapshot_message(&seed_snapshot) {
                        Ok(message) => message,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to encode endpoint snapshot");
                            return false;
                        }
                    };
                connection.shell_location = Some(location);
                connection.shell_snapshot = Some(seed_snapshot);
                self.clients.insert(client_id, connection);
                if self.app.state.popup_pane.is_some() && self.popup_owner_tab_id.is_none() {
                    self.popup_owner_tab_id = self.shell_tab_id_for_client(client_id);
                }
                self.send_to_client(client_id, snapshot_message);
                self.foreground_client_id = Some(client_id);
                if first_app_client {
                    self.app.mark_git_status_refresh_due(Instant::now());
                }
                self.sync_foreground_client_state();
                self.claim_unowned_shell_tab_geometry(client_id, true);
                self.nudge_handoff_panes_on_first_client_attach();
                true
            }
            ServerEvent::GraphicsTransmissionResult {
                client_id,
                transfer_id,
                image_id,
                success,
            } => self.complete_direct_graphics(client_id, transfer_id, image_id, success),
            ServerEvent::GraphicsTransmissionStarted {
                client_id,
                transfer_id,
                image_id,
            } => self.start_direct_graphics_response(client_id, transfer_id, image_id),
            ServerEvent::ClientAttachTerminal {
                client_id,
                terminal_id,
                takeover,
            } => self.attach_terminal_client(client_id, terminal_id, takeover),
            ServerEvent::ClientObserveTerminal { client_id, target } => {
                self.observe_terminal_client(client_id, target)
            }
            ServerEvent::ClientControlTerminal {
                client_id,
                target,
                takeover,
            } => self.control_terminal_client(client_id, target, takeover),
            ServerEvent::ClientAttachScroll {
                client_id,
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            } => self.handle_terminal_attach_scroll(
                client_id, source, direction, lines, column, row, modifiers,
            ),
            ServerEvent::ClientAttachMouse {
                client_id,
                kind,
                position,
                geometry,
                modifiers,
                lines,
            } => self.handle_terminal_attach_mouse(
                client_id, kind, position, geometry, modifiers, lines,
            ),
            ServerEvent::ClientInput { client_id, data } => {
                if self.handoff_in_progress {
                    debug!(
                        client_id,
                        len = data.len(),
                        "ignored direct terminal input during handoff"
                    );
                    return false;
                }
                let Some(ClientConnection {
                    mode: ClientConnectionMode::TerminalAttach { terminal_id },
                    ..
                }) = self.clients.get(&client_id)
                else {
                    return false;
                };
                if let Some(runtime) = self.runtime_for_terminal_id_string(terminal_id) {
                    if let Err(err) = apply_terminal_attach_input(runtime, data) {
                        warn!(client_id, terminal_id = %terminal_id, err = %err);
                    }
                }
                true
            }
            ServerEvent::ClientPasteRejected {
                client_id,
                size,
                max,
            } => {
                let detail = format!("Input message is {size} bytes; Herdr's limit is {max} bytes");
                let message = if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::ClientShell)
                ) {
                    ServerMessage::ClientShellError {
                        message: format!("Paste rejected: {detail}"),
                    }
                } else {
                    ServerMessage::Notify {
                        kind: protocol::NotifyKind::Toast,
                        message: "Paste rejected".to_owned(),
                        body: Some(detail),
                    }
                };
                self.send_to_client(client_id, message);
                false
            }
            ServerEvent::ClientClipboardImage {
                client_id,
                target,
                extension,
                data,
            } => {
                debug!(
                    client_id,
                    len = data.len(),
                    extension = %extension,
                    "client clipboard image received"
                );
                if !self.client_clipboard_image_target_is_valid(client_id, &target) {
                    return false;
                }
                match self.stage_client_clipboard_image(client_id, &extension, &data) {
                    Ok(staged) => {
                        let routed = self.paste_client_clipboard_image_path(
                            client_id,
                            target,
                            staged.paste_text,
                        );
                        if routed {
                            if let Some(client) = self.clients.get_mut(&client_id) {
                                client.staged_clipboard_files.push(staged.path);
                            } else {
                                crate::server::clipboard_image::remove_files(vec![staged.path]);
                                return false;
                            }
                        } else {
                            crate::server::clipboard_image::remove_files(vec![staged.path]);
                        }
                        routed
                    }
                    Err(err) => {
                        warn!(client_id, err = %err, "failed to stage client clipboard image");
                        true
                    }
                }
            }
            ServerEvent::ClientResize {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
            } => {
                info!(
                    client_id,
                    cols, rows, cell_width_px, cell_height_px, pixel_mouse, "client resize"
                );
                let observed = crate::kitty_graphics::HostCellSize {
                    width_px: cell_width_px,
                    height_px: cell_height_px,
                };
                let pixel_mouse = pixel_mouse && observed.is_known();
                let direct_terminal_id = if let Some(ClientConnection {
                    mode: ClientConnectionMode::TerminalAttach { terminal_id },
                    terminal_size,
                    cell_size,
                    pixel_mouse: client_pixel_mouse,
                    render_state,
                    ..
                }) = self.clients.get_mut(&client_id)
                {
                    *terminal_size = (cols, rows);
                    *cell_size = observed;
                    *client_pixel_mouse = pixel_mouse;
                    render_state.request_repaint();
                    Some((terminal_id.clone(), *cell_size))
                } else {
                    None
                };
                if let Some((terminal_id, cell_size)) = direct_terminal_id {
                    if let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) {
                        runtime.resize(rows, cols, cell_size.width_px, cell_size.height_px);
                    }
                    return true;
                }
                if let Some(ClientConnection {
                    mode:
                        ClientConnectionMode::TerminalObserve { .. }
                        | ClientConnectionMode::TerminalPending,
                    terminal_size,
                    cell_size,
                    pixel_mouse: client_pixel_mouse,
                    render_state,
                    ..
                }) = self.clients.get_mut(&client_id)
                {
                    *terminal_size = (cols, rows);
                    *cell_size = observed;
                    *client_pixel_mouse = pixel_mouse;
                    render_state.request_repaint();
                    return true;
                }
                false
            }
            ServerEvent::ClientShellResize {
                client_id,
                surface_cols,
                surface_rows,
                cell_width_px,
                cell_height_px,
                pixel_mouse,
            } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                if !matches!(client.mode, ClientConnectionMode::ClientShell) {
                    return false;
                }
                client.terminal_size = (surface_cols, surface_rows);
                let observed = crate::kitty_graphics::HostCellSize {
                    width_px: cell_width_px,
                    height_px: cell_height_px,
                };
                if observed.is_known() {
                    client.cell_size = observed;
                }
                client.pixel_mouse = pixel_mouse && observed.is_known();
                client.request_repaint();
                self.promote_client_to_foreground(client_id);
                self.resize_shell_tab_if_controller(client_id, true);
                true
            }
            ServerEvent::ClientShellHostTheme { client_id, update } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                if !matches!(client.mode, ClientConnectionMode::ClientShell) {
                    return false;
                }
                if !client.update_host_theme(&update) {
                    return false;
                }
                if self.foreground_client_id != Some(client_id) {
                    return false;
                }
                let mut changed = self.app.set_host_terminal_appearance_state(
                    client.host_terminal_appearance,
                    client.host_terminal_appearance_explicit,
                );
                changed |= self.app.set_host_terminal_theme(client.host_terminal_theme);
                if changed {
                    self.resize_shared_runtime_to_effective_size_before_input();
                }
                changed
            }
            ServerEvent::ClientShellFocus { client_id, focused } => {
                let Some(client) = self.clients.get(&client_id) else {
                    return false;
                };
                if !matches!(client.mode, ClientConnectionMode::ClientShell)
                    || client.outer_terminal_focus == Some(focused)
                {
                    return false;
                }
                let tab_id = self.shell_tab_id_for_client(client_id);
                let another_focused_viewer = self.clients.iter().any(|(&other_id, client)| {
                    other_id != client_id
                        && client.is_shell_client()
                        && client.outer_terminal_focus == Some(true)
                        && self.shell_tab_id_for_client(other_id) == tab_id
                });
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.outer_terminal_focus = Some(focused);
                }
                if focused {
                    self.promote_client_to_foreground(client_id);
                    self.claim_shell_tab_geometry(client_id, false);
                    if !another_focused_viewer {
                        if let Some(target) = self.shell_focus_target(client_id) {
                            self.send_shell_focus_target(
                                &target,
                                crate::ghostty::FocusEvent::Gained,
                            );
                        }
                    }
                    true
                } else {
                    if self.foreground_client_id == Some(client_id) {
                        self.app.state.outer_terminal_focus = Some(false);
                    }
                    if !another_focused_viewer {
                        if let Some(target) = self.shell_focus_target(client_id) {
                            self.send_shell_focus_target(&target, crate::ghostty::FocusEvent::Lost);
                        }
                    }
                    true
                }
            }
            ServerEvent::ClientShellMouseCapture { client_id, enabled } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                if !matches!(client.mode, ClientConnectionMode::ClientShell)
                    || client.shell_mouse_capture == enabled
                {
                    return false;
                }
                client.shell_mouse_capture = enabled;
                client.host_mouse_capture_active = None;
                true
            }
            ServerEvent::ClientShellPaneInput {
                client_id,
                pane_id,
                events,
            } => {
                if self.handoff_in_progress
                    || !self.clients.get(&client_id).is_some_and(|client| {
                        matches!(client.mode, ClientConnectionMode::ClientShell)
                    })
                {
                    return false;
                }
                let pixel_mouse = self.clients.get(&client_id).is_some_and(|client| {
                    client.pixel_mouse && client.host_sgr_pixels_active == Some(true)
                });
                let mut events = events;
                let Some((workspace_index, runtime_pane_id)) = self.app.parse_pane_id(&pane_id)
                else {
                    return false;
                };
                let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                    &self.app.terminal_runtimes,
                    workspace_index,
                    runtime_pane_id,
                ) else {
                    return false;
                };
                super::pane_input::downgrade_ineligible_pixel_mouse(
                    &mut events,
                    pixel_mouse,
                    runtime.current_size(),
                    runtime.pixel_size(),
                );
                let popup_blocks_input = self.app.state.popup_pane.is_some()
                    && self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id);
                if popup_blocks_input
                    || !self.shell_client_views_pane(client_id, workspace_index, runtime_pane_id)
                {
                    let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                        &self.app.terminal_runtimes,
                        workspace_index,
                        runtime_pane_id,
                    ) else {
                        return false;
                    };
                    let releases = events
                        .into_iter()
                        .filter(client_pane_input_releases_press)
                        .collect::<Vec<_>>();
                    if releases.is_empty() {
                        return false;
                    }
                    if let Some(client) = self.clients.get_mut(&client_id) {
                        client.track_shell_input(
                            ClientShellInputTarget::Pane(pane_id.clone()),
                            &releases,
                        );
                    }
                    let scroll_before = runtime.scroll_metrics();
                    if let Err(err) = apply_client_pane_input_events(runtime, &releases) {
                        warn!(client_id, pane_id, err = %err, "targeted client shell release failed");
                    }
                    return runtime.scroll_metrics() != scroll_before;
                }
                let interaction = client_pane_input_has_interaction(&events);
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client
                        .track_shell_input(ClientShellInputTarget::Pane(pane_id.clone()), &events);
                }
                let foreground_changed =
                    interaction && self.promote_client_to_foreground(client_id);
                let geometry_changed =
                    interaction && self.claim_shell_tab_geometry(client_id, false);
                let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                    &self.app.terminal_runtimes,
                    workspace_index,
                    runtime_pane_id,
                ) else {
                    return foreground_changed | geometry_changed;
                };
                let scroll_before = runtime.scroll_metrics();
                if let Err(err) = apply_client_pane_input_events(runtime, &events) {
                    warn!(client_id, pane_id, err = %err, "targeted client shell input failed");
                }
                foreground_changed | geometry_changed || runtime.scroll_metrics() != scroll_before
            }
            ServerEvent::ClientShellPopupInput {
                client_id,
                terminal_id,
                events,
            } => {
                if self.handoff_in_progress
                    || !self.clients.get(&client_id).is_some_and(|client| {
                        matches!(client.mode, ClientConnectionMode::ClientShell)
                    })
                {
                    return false;
                }
                let pixel_mouse = self.clients.get(&client_id).is_some_and(|client| {
                    client.pixel_mouse && client.host_sgr_pixels_active == Some(true)
                });
                let mut events = events;
                let Some(popup_terminal_id) = self
                    .app
                    .state
                    .popup_pane
                    .as_ref()
                    .map(|popup| popup.terminal_id.clone())
                else {
                    return false;
                };
                if popup_terminal_id.as_str() != terminal_id {
                    return false;
                }
                let Some(runtime) = self.app.terminal_runtimes.get(&popup_terminal_id) else {
                    return false;
                };
                super::pane_input::downgrade_ineligible_pixel_mouse(
                    &mut events,
                    pixel_mouse,
                    runtime.current_size(),
                    runtime.pixel_size(),
                );
                if self.popup_owner_tab_id != self.shell_tab_id_for_client(client_id) {
                    let releases = events
                        .into_iter()
                        .filter(client_pane_input_releases_press)
                        .collect::<Vec<_>>();
                    if releases.is_empty() {
                        return false;
                    }
                    if let Some(client) = self.clients.get_mut(&client_id) {
                        client.track_shell_input(
                            ClientShellInputTarget::Popup(terminal_id.clone()),
                            &releases,
                        );
                    }
                    let scroll_before = runtime.scroll_metrics();
                    if let Err(err) = apply_client_popup_input_events(runtime, &releases) {
                        warn!(client_id, terminal_id, err = %err, "targeted client popup release failed");
                    }
                    return runtime.scroll_metrics() != scroll_before;
                }
                let interaction = client_pane_input_has_interaction(&events);
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.track_shell_input(
                        ClientShellInputTarget::Popup(terminal_id.clone()),
                        &events,
                    );
                }
                let foreground_changed =
                    interaction && self.promote_client_to_foreground(client_id);
                let geometry_changed =
                    interaction && self.claim_shell_tab_geometry(client_id, false);
                let Some(runtime) = self.app.terminal_runtimes.get(&popup_terminal_id) else {
                    return foreground_changed | geometry_changed;
                };
                let scroll_before = runtime.scroll_metrics();
                if let Err(err) = apply_client_popup_input_events(runtime, &events) {
                    warn!(client_id, terminal_id, err = %err, "targeted client popup input failed");
                }
                foreground_changed | geometry_changed || runtime.scroll_metrics() != scroll_before
            }
            ServerEvent::ClientShellEndpointRequestError {
                client_id,
                boot_id,
                request_id,
                code,
                message,
            } => {
                let Some(client) = self.clients.get(&client_id) else {
                    return false;
                };
                if !matches!(client.mode, ClientConnectionMode::ClientShell) {
                    self.remove_client_and_resize_if_needed(client_id);
                    return true;
                }
                let message = crate::server::client_commands::error_message(
                    boot_id, request_id, code, message,
                );
                self.send_to_client(client_id, message);
                false
            }
            ServerEvent::ClientShellEndpointRequest {
                client_id,
                boot_id,
                mut request,
            } => {
                let Some(client) = self.clients.get(&client_id) else {
                    return false;
                };
                if !matches!(client.mode, ClientConnectionMode::ClientShell) {
                    self.remove_client_and_resize_if_needed(client_id);
                    return true;
                }
                let request_id = request.id.clone();
                if !crate::server::client_commands::supports_client_shell_method(&request.method) {
                    let message = crate::server::client_commands::error_message(
                        boot_id,
                        request_id,
                        "unsupported_endpoint_command",
                        "this method is not available through the client shell command lane",
                    );
                    self.send_to_client(client_id, message);
                    return false;
                }
                if boot_id != self.client_shell_boot_id {
                    let message = crate::server::client_commands::error_message(
                        boot_id,
                        request_id,
                        "stale_boot",
                        "endpoint command targeted an earlier server boot",
                    );
                    self.send_to_client(client_id, message);
                    return false;
                }
                if client.shell_endpoint_command_in_flight {
                    self.send_to_client(
                        client_id,
                        ServerMessage::ServerShutdown {
                            reason: Some("client sent concurrent endpoint commands".into()),
                        },
                    );
                    self.remove_client_and_resize_if_needed(client_id);
                    return true;
                }

                let api_request_id = format!(
                    "endpoint:{}:{client_id}:{request_id}",
                    self.client_shell_boot_id
                );
                request.id = api_request_id.clone();
                let (respond_to, response_rx) = std::sync::mpsc::channel();
                if let Err(err) = crate::server::client_commands::spawn_response_waiter(
                    client_id,
                    boot_id.clone(),
                    request_id.clone(),
                    response_rx,
                    self.server_event_tx.clone(),
                ) {
                    let message = crate::server::client_commands::error_message(
                        boot_id,
                        request_id,
                        "server_unavailable",
                        format!("failed to start endpoint response bridge: {err}"),
                    );
                    self.send_to_client(client_id, message);
                    return false;
                }
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.shell_endpoint_command_in_flight = true;
                    let deferred_worktree = matches!(
                        &request.method,
                        api::schema::Method::WorktreeCreate(_)
                            | api::schema::Method::WorktreeRemove(_)
                    );
                    let deferred_navigation = matches!(
                        &request.method,
                        api::schema::Method::WorktreeCreate(params) if params.focus
                    );
                    client.shell_deferred_navigation_request_id =
                        deferred_worktree.then(|| api_request_id.clone());
                    client.shell_deferred_navigation_response = deferred_navigation.then(Vec::new);
                }
                let foreground_changed = self.promote_client_to_foreground(client_id);
                foreground_changed
                    | self.handle_client_shell_api_request(
                        client_id,
                        api::ApiRequestMessage {
                            request: *request,
                            respond_to,
                            response_write_complete: None,
                            stream_active: None,
                        },
                    )
            }
            ServerEvent::ClientShellEndpointResponseChunkReady {
                client_id,
                boot_id,
                request_id,
                final_chunk,
                data,
            } => {
                let valid = self.clients.get(&client_id).is_some_and(|client| {
                    matches!(client.mode, ClientConnectionMode::ClientShell)
                        && client.shell_endpoint_command_in_flight
                        && boot_id == self.client_shell_boot_id
                });
                if !valid {
                    return false;
                }
                let completed_deferred_response =
                    self.clients.get_mut(&client_id).and_then(|client| {
                        let response = client.shell_deferred_navigation_response.as_mut()?;
                        response.extend_from_slice(&data);
                        final_chunk
                            .then(|| client.shell_deferred_navigation_response.take())
                            .flatten()
                    });
                if final_chunk {
                    if let Some(client) = self.clients.get_mut(&client_id) {
                        client.shell_endpoint_command_in_flight = false;
                        client.shell_deferred_navigation_request_id = None;
                    }
                }
                let deferred_tab_id = completed_deferred_response
                    .as_deref()
                    .and_then(Self::deferred_endpoint_navigation_tab_id);
                let focus_before = self.shell_focus_target(client_id);
                let focused_tabs_before = self.focused_shell_tabs();
                let navigation_changed = deferred_tab_id
                    .as_deref()
                    .is_some_and(|tab_id| self.focus_shell_client_on_tab(client_id, tab_id));
                let geometry_changed =
                    navigation_changed && self.claim_shell_tab_geometry(client_id, false);
                if navigation_changed {
                    self.reconcile_client_shell_locations();
                    let focus_after = self.shell_focus_target(client_id);
                    let focused_tabs_after = self.focused_shell_tabs();
                    self.app.accept_current_focus_without_events();
                    self.send_shell_navigation_focus_events(
                        focus_before.as_ref(),
                        focus_after.as_ref(),
                        &focused_tabs_before,
                        &focused_tabs_after,
                    );
                }
                self.send_to_client(
                    client_id,
                    ServerMessage::ClientShellEndpointResponseChunk {
                        boot_id,
                        request_id,
                        final_chunk,
                        data,
                    },
                );
                navigation_changed | geometry_changed
            }
            ServerEvent::ClientDetach { client_id } => {
                info!(client_id, "client detached");
                self.send_terminal_stream_detach_shutdown(client_id);
                self.remove_client_and_resize_if_needed(client_id);
                true
            }
            ServerEvent::ClientDisconnected { client_id } => {
                info!(client_id, "client disconnected");
                self.remove_client_and_resize_if_needed(client_id);
                true
            }
            ServerEvent::ClientWriterDrained { client_id } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                client.take_deferred_render() != DeferredRender::None
            }
            ServerEvent::QuitSignal => {
                // The quit check at the top of the loop handles this.
                // No render needed — the next iteration will initiate shutdown.
                false
            }
        }
    }

    fn handle_server_event_with_render_impact(&mut self, ev: ServerEvent) -> RenderImpact {
        if self.handle_server_event(ev) {
            RenderImpact::Full
        } else {
            RenderImpact::None
        }
    }

    fn ignore_client_event_during_handoff(ev: &ServerEvent) -> bool {
        !matches!(
            ev,
            ServerEvent::ClientConnected { .. }
                | ServerEvent::ClientShellConnected { .. }
                | ServerEvent::ClientShellEndpointResponseChunkReady { .. }
                | ServerEvent::ClientDisconnected { .. }
                | ServerEvent::ClientWriterDrained { .. }
                | ServerEvent::QuitSignal
        )
    }

    fn agent_read_not_idle_error(
        &self,
        request: &api::schema::Request,
    ) -> Option<api::schema::ErrorBody> {
        use api::schema::{Method, ReadFormat, ReadSource};

        let Method::AgentRead(params) = &request.method else {
            return None;
        };
        let requested = params.lines?;
        if params.format != ReadFormat::Text
            || !matches!(
                params.source,
                ReadSource::Recent | ReadSource::RecentUnwrapped
            )
        {
            return None;
        }
        let target = self.app.resolve_agent_target(&params.target).ok()?;
        let terminal = self
            .app
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.as_str() == target.terminal_id)?;
        if terminal.effective_known_agent().is_none()
            || terminal.state == crate::detect::AgentState::Idle
        {
            return None;
        }
        let runtime = self.app.terminal_runtimes.get(&terminal.id)?;
        let (screen, snapshot) = runtime.screen_text_snapshot()?;
        if screen != crate::ghostty::ActiveScreen::Alternate
            || snapshot.rows.len() >= requested.min(1000) as usize
        {
            return None;
        }
        let status = crate::detect::manifest::agent_state_label(terminal.state);
        Some(api::schema::ErrorBody {
            code: "agent_not_idle".into(),
            message: format!(
                "cannot read {requested} lines while {} is {status}: its alternate-screen history can only be captured by scrolling while idle. Wait and retry, or use --source visible",
                params.target
            ),
        })
    }

    fn alt_screen_read_spec(&self, request: &api::schema::Request) -> Option<AltScreenReadSpec> {
        use api::schema::{Method, ReadFormat, ReadIntent, ReadSource};

        let (target, source, lines, format) = match &request.method {
            Method::AgentRead(params) => (
                self.app.resolve_agent_target(&params.target).ok()?,
                params.source,
                params.lines,
                params.format,
            ),
            Method::PaneRead(params) if params.intent == ReadIntent::Interactive => (
                self.app.resolve_terminal_target(&params.pane_id).ok()?,
                params.source,
                params.lines,
                params.format,
            ),
            _ => return None,
        };
        if format != ReadFormat::Text
            || !matches!(source, ReadSource::Recent | ReadSource::RecentUnwrapped)
        {
            return None;
        }
        let lines = lines.unwrap_or(80).min(1000) as usize;
        if lines == 0
            || self
                .terminal_attach_owners
                .contains_key(target.terminal_id.as_str())
            || self
                .pending_alt_screen_reads
                .iter()
                .any(|pending| pending.terminal_id.as_str() == target.terminal_id)
        {
            return None;
        }
        let terminal = self
            .app
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.as_str() == target.terminal_id)?;
        if terminal.effective_known_agent().is_none()
            || terminal.state != crate::detect::AgentState::Idle
        {
            return None;
        }
        let runtime = self.app.terminal_runtimes.get(&terminal.id)?;
        if runtime.wheel_routing() != Some(crate::pane::WheelRouting::MouseReport) {
            return None;
        }
        let (screen, initial, content_seq) = runtime.screen_text_snapshot_with_seq()?;
        if screen != crate::ghostty::ActiveScreen::Alternate || initial.rows.len() >= lines {
            return None;
        }
        Some(AltScreenReadSpec {
            terminal_id: terminal.id.clone(),
            lines,
            unwrap: source == ReadSource::RecentUnwrapped,
            initial,
            content_seq,
        })
    }

    fn poll_pending_alt_screen_reads(&mut self, now: Instant) {
        let pending = std::mem::take(&mut self.pending_alt_screen_reads);
        for read in pending {
            let runtime = self.app.terminal_runtimes.get(&read.terminal_id);
            let remains_idle = self
                .app
                .state
                .terminals
                .get(&read.terminal_id)
                .is_some_and(|terminal| terminal.state == crate::detect::AgentState::Idle);
            let attached = self
                .terminal_attach_owners
                .contains_key(read.terminal_id.as_str());
            let outcome = if remains_idle && !attached {
                read.poll(runtime, now)
            } else {
                read.abort(runtime, now)
            };
            if let Some(read) = outcome {
                self.pending_alt_screen_reads.push(read);
            }
        }
    }

    fn alt_screen_read_conflict(&self, request: &api::schema::Request) -> AltScreenReadConflict {
        let (target, source, lines, format) = match &request.method {
            api::schema::Method::AgentRead(params) => (
                self.app.resolve_agent_target(&params.target).ok(),
                params.source,
                params.lines,
                params.format,
            ),
            api::schema::Method::PaneRead(params) => (
                self.app.resolve_terminal_target(&params.pane_id).ok(),
                params.source,
                params.lines,
                params.format,
            ),
            _ => return AltScreenReadConflict::None,
        };
        let Some(target) = target else {
            return AltScreenReadConflict::None;
        };
        let Some(pending) = self
            .pending_alt_screen_reads
            .iter()
            .find(|pending| pending.terminal_id.as_str() == target.terminal_id)
        else {
            return AltScreenReadConflict::None;
        };
        if format == api::schema::ReadFormat::Text {
            AltScreenReadConflict::Frozen(pending.frozen_snapshot(source, lines))
        } else {
            AltScreenReadConflict::Defer
        }
    }

    fn process_deferred_alt_screen_reads(&mut self) -> bool {
        let deferred = std::mem::take(&mut self.deferred_alt_screen_reads);
        let mut changed = false;
        for msg in deferred {
            match self.alt_screen_read_conflict(&msg.request) {
                AltScreenReadConflict::None => {
                    changed |= self.handle_api_request_with_shutdown_check(msg);
                }
                AltScreenReadConflict::Frozen(_) | AltScreenReadConflict::Defer => {
                    self.deferred_alt_screen_reads.push(msg);
                }
            }
        }
        changed
    }

    /// Drains API requests with shutdown awareness.
    ///
    /// During shutdown, remaining requests get a `server_unavailable` error.
    fn drain_api_requests_with_shutdown_check(&mut self) -> bool {
        let mut changed = false;
        while !self.should_quit.load(Ordering::Acquire) {
            let Ok(msg) = self.app.api_rx.try_recv() else {
                break;
            };
            changed |= self.handle_api_request_with_shutdown_check(msg);
        }
        changed
    }

    fn reject_queued_api_requests_for_shutdown(&mut self) {
        for _ in 0..self.app.api_rx.len() {
            let Ok(msg) = self.app.api_rx.try_recv() else {
                break;
            };
            self.handle_api_request_with_shutdown_check(msg);
        }
    }

    fn drain_api_requests_with_render_impact(&mut self) -> RenderImpact {
        let mut impact = RenderImpact::None;
        while !self.should_quit.load(Ordering::Acquire) {
            let Ok(msg) = self.app.api_rx.try_recv() else {
                break;
            };
            impact.merge(self.handle_api_request_with_render_impact(msg));
        }
        impact
    }

    fn handle_api_request_with_render_impact(
        &mut self,
        msg: api::ApiRequestMessage,
    ) -> RenderImpact {
        if matches!(
            &msg.request.method,
            api::schema::Method::PaneGraphicsStreamSet(_)
                | api::schema::Method::PaneGraphicsStreamDirect(_)
        ) {
            return self.handle_pane_graphics_stream_frame(msg);
        }
        if self.handle_api_request_with_shutdown_check(msg) {
            RenderImpact::Full
        } else {
            RenderImpact::None
        }
    }

    fn handle_api_request_with_shutdown_check_inner(
        &mut self,
        msg: api::ApiRequestMessage,
        skip_default_workspace_for_request: bool,
    ) -> bool {
        if self.shutting_down {
            // During shutdown, respond with server_unavailable.
            let response = serde_json::to_string(&api::schema::ErrorResponse {
                id: msg.request.id,
                error: api::schema::ErrorBody {
                    code: "server_unavailable".into(),
                    message: "server is shutting down".into(),
                },
            })
            .unwrap_or_else(|_| {
                r#"{"id":"","error":{"code":"server_unavailable","message":"server is shutting down"}}"#
                    .to_string()
            });
            let _ = msg.respond_to.send(response);
            return false;
        }

        let frozen_alt_screen_read = match self.alt_screen_read_conflict(&msg.request) {
            AltScreenReadConflict::None => None,
            AltScreenReadConflict::Frozen(snapshot) => Some(snapshot),
            AltScreenReadConflict::Defer => {
                self.deferred_alt_screen_reads.push(msg);
                return false;
            }
        };

        let metadata_expired = self.app.expire_due_metadata(Instant::now());
        let stream_open = match &msg.request.method {
            api::schema::Method::PaneGraphicsStreamOpen(params) => Some(params.clone()),
            _ => None,
        };
        let stream_active = msg.stream_active.clone();

        if let api::schema::Method::ServerLiveHandoff(params) = &msg.request.method {
            let handoff_result = self.perform_live_handoff(params.clone());
            let handoff_succeeded = handoff_result.is_ok();
            let response = match handoff_result {
                Ok(()) => serde_json::to_string(&api::schema::SuccessResponse {
                    id: msg.request.id,
                    result: api::schema::ResponseResult::Ok {},
                }),
                Err(err) => serde_json::to_string(&api::schema::ErrorResponse {
                    id: msg.request.id,
                    error: api::schema::ErrorBody {
                        code: "handoff_failed".into(),
                        message: err.to_string(),
                    },
                }),
            }
            .unwrap_or_else(|_| "{}".to_string());
            let _ = msg.respond_to.send(response);
            if handoff_succeeded {
                wait_for_live_handoff_response_write(msg.response_write_complete);
                self.finish_live_handoff_shutdown();
            }
            return true;
        }

        if let api::schema::Method::NotificationShow(params) = &msg.request.method {
            let response =
                self.handle_notification_show_api(msg.request.id.clone(), params.clone());
            let _ = msg.respond_to.send(response);
            return true;
        }

        match &msg.request.method {
            api::schema::Method::ClientWindowTitleSet(params) => {
                let response = self.handle_client_window_title_api(
                    msg.request.id.clone(),
                    Some(params.title.clone()),
                );
                let _ = msg.respond_to.send(response);
                return true;
            }
            api::schema::Method::ClientWindowTitleClear(_) => {
                let response = self.handle_client_window_title_api(msg.request.id.clone(), None);
                let _ = msg.respond_to.send(response);
                return true;
            }
            _ => {}
        }

        let pane_graphics_revision_before = matches!(
            &msg.request.method,
            api::schema::Method::PaneGraphicsSet(_)
                | api::schema::Method::PaneGraphicsClear(_)
                | api::schema::Method::PaneGraphicsStreamOpen(_)
                | api::schema::Method::PaneGraphicsStreamClose(_)
        )
        .then_some(self.app.pane_graphics.revision());
        let mut changed = metadata_expired
            | (pane_graphics_revision_before.is_none() && api::request_changes_ui(&msg.request));
        let skip_default_workspace = skip_default_workspace_for_request
            || matches!(
                &msg.request.method,
                api::schema::Method::ServerStop(_) | api::schema::Method::ServerLiveHandoff(_)
            );
        changed |= self.drain_all_internal_events_with_forwarding();

        // Capture toast and effective pane states before the API call so we can
        // forward resulting client-local notifications. API requests like
        // pane.report_agent trigger handle_internal_event internally, which
        // bypasses drain_internal_events_with_forwarding. Headless mode disables
        // local sound playback, so sound notifications need to be forwarded here.
        let toast_before = self.app.state.toast.clone();
        let pane_states_before: Vec<(
            usize,
            crate::layout::PaneId,
            crate::detect::AgentState,
            Option<String>,
        )> = {
            let terminals = &self.app.state.terminals;
            self.app
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs.iter().flat_map(move |tab| {
                        tab.panes.iter().filter_map(move |(&pane_id, pane)| {
                            terminals.get(&pane.attached_terminal_id).map(|terminal| {
                                (
                                    ws_idx,
                                    pane_id,
                                    terminal.state,
                                    terminal.effective_agent_label().map(str::to_string),
                                )
                            })
                        })
                    })
                })
                .collect()
        };

        self.sync_foreground_client_state();
        if let Some(error) = self.agent_read_not_idle_error(&msg.request) {
            let response = serde_json::to_string(&api::schema::ErrorResponse {
                id: msg.request.id.clone(),
                error,
            })
            .unwrap_or_else(|_| "{}".to_owned());
            let _ = msg.respond_to.send(response);
            return changed;
        }
        let alt_screen_read_spec = self.alt_screen_read_spec(&msg.request);
        if matches!(&msg.request.method, api::schema::Method::AgentPrompt(_)) {
            let deferred_changed = self
                .app
                .handle_deferred_agent_api_request(msg.request, msg.respond_to);
            return changed | deferred_changed;
        }
        if matches!(
            &msg.request.method,
            api::schema::Method::WorktreeCreate(_) | api::schema::Method::WorktreeRemove(_)
        ) {
            let deferred_changed = self
                .app
                .handle_deferred_worktree_api_request(msg.request, msg.respond_to);
            return changed | deferred_changed;
        }
        if self.foreground_client_id.is_some_and(|client_id| {
            self.clients
                .get(&client_id)
                .is_some_and(|client| matches!(client.mode, ClientConnectionMode::ClientShell))
        }) {
            self.app.state.view.terminal_area =
                Rect::new(0, 0, self.effective_size.0, self.effective_size.1);
        }
        let mut response = if matches!(
            &msg.request.method,
            api::schema::Method::ServerReloadConfig(_)
        ) {
            let report = self.reload_server_config(true);
            serde_json::to_string(&api::schema::SuccessResponse {
                id: msg.request.id.clone(),
                result: api::schema::ResponseResult::ConfigReload {
                    status: report.status,
                    diagnostics: report.diagnostics,
                },
            })
            .unwrap_or_else(|err| {
                serde_json::to_string(&api::schema::ErrorResponse {
                    id: String::new(),
                    error: api::schema::ErrorBody {
                        code: "serialization_error".into(),
                        message: err.to_string(),
                    },
                })
                .unwrap_or_else(|_| "{}".to_string())
            })
        } else {
            self.app
                .handle_api_request_after_internal_events_drained(msg.request)
        };
        if let Some(snapshot) = frozen_alt_screen_read {
            if let Ok(mut success) = serde_json::from_str::<api::schema::SuccessResponse>(&response)
            {
                if let api::schema::ResponseResult::PaneRead { read } = &mut success.result {
                    read.text = snapshot.text;
                    read.truncated = snapshot.truncated;
                    if let Ok(serialized) = serde_json::to_string(&success) {
                        response = serialized;
                    }
                }
            }
        }
        if let (Some(params), Some(active)) = (stream_open.as_ref(), stream_active) {
            self.app
                .attach_pane_graphics_stream_active(params, active, &response);
        }
        if let Some(spec) = alt_screen_read_spec {
            if let Ok(success) = serde_json::from_str::<api::schema::SuccessResponse>(&response) {
                if let api::schema::ResponseResult::PaneRead { read } = success.result {
                    let pending = crate::server::alt_screen_read::PendingAltScreenRead::start(
                        spec.terminal_id,
                        success.id,
                        msg.respond_to,
                        response,
                        read,
                        spec.lines,
                        spec.unwrap,
                        spec.initial,
                        spec.content_seq,
                        Instant::now(),
                    );
                    self.pending_alt_screen_reads.push(pending);
                    return changed;
                }
            }
        }
        let _ = msg.respond_to.send(response);

        if let Some(revision_before) = pane_graphics_revision_before {
            changed |= revision_before != self.app.pane_graphics.revision();
        }

        // Forward new toast state only when a client-local delivery mode is selected.
        // Herdr delivery renders the toast in-frame and must not ask clients to
        // show a terminal or system notification.
        let toast_after = self.app.state.toast.clone();
        let forwarded_toast_from_state = if should_forward_toast_to_clients(
            self.app.state.toast_config.delivery,
        ) && toast_after.is_some()
            && toast_after != toast_before
        {
            if let Some(toast) = &toast_after {
                debug!(title = %toast.title, body = %toast.context, "forwarding toast notification from API request");
                self.send_notify_to_foreground_client(
                    toast_notify_kind(self.app.state.toast_config.delivery)
                        .expect("toast forwarding requires a client notification kind"),
                    &toast.title,
                    non_empty_body(&toast.context),
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        // Forward notifications for effective pane state changes that occurred
        // during the API request. Hook authority is already folded into
        // pane.state, so raw hook transitions must not produce separate sounds.
        for (ws_idx, pane_id, prev_state, prev_agent_label) in &pane_states_before {
            let pane_after = self
                .app
                .state
                .workspaces
                .get(*ws_idx)
                .and_then(|ws| ws.tabs.iter().find_map(|tab| tab.panes.get(pane_id)));

            let Some(pane_after) = pane_after else {
                continue;
            };
            let terminal_id = pane_after.attached_terminal_id.clone();

            let Some(terminal_after) = self.app.state.terminals.get(&terminal_id) else {
                continue;
            };

            let new_state = terminal_after.state;
            if new_state == *prev_state {
                continue;
            }

            let is_active_tab = self.app.state.pane_is_in_active_tab(*ws_idx, *pane_id);
            let suppress_active_tab_notifications =
                self.active_tab_suppresses_notifications(is_active_tab);

            let agent = terminal_after.effective_known_agent();
            let agent_label = terminal_after.effective_agent_label().map(str::to_string);

            debug!(
                ws_idx,
                pane_id = pane_id.raw(),
                prev_state = ?prev_state,
                new_state = ?new_state,
                agent = ?agent,
                "pane effective state changed during API request, checking notification"
            );
            self.forward_semantic_agent_transition(
                *ws_idx,
                *pane_id,
                *prev_state,
                new_state,
                prev_agent_label.as_deref(),
                agent_label.as_deref(),
                agent,
            );

            if !forwarded_toast_from_state
                && self.app.state.toast_config.delay_seconds == 0
                && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
            {
                if let Some(kind) =
                    crate::app::actions::notification_toast_for_state_change_with_agent_labels(
                        suppress_active_tab_notifications,
                        *prev_state,
                        new_state,
                        prev_agent_label.as_deref(),
                        agent_label.as_deref(),
                    )
                {
                    if let Some(agent_label) = self
                        .app
                        .state
                        .terminals
                        .get(&terminal_id)
                        .and_then(|terminal| terminal.effective_agent_label())
                    {
                        let event_text = match kind {
                            crate::app::state::ToastKind::NeedsAttention => "needs attention",
                            crate::app::state::ToastKind::Finished => "finished",
                            crate::app::state::ToastKind::UpdateInstalled => "updated",
                        };
                        let workspace_label = self.app.state.workspaces[*ws_idx].display_name_from(
                            &self.app.state.terminals,
                            &self.app.terminal_runtimes,
                        );
                        let context = crate::app::actions::notification_context(
                            &self.app.state.workspaces[*ws_idx],
                            &workspace_label,
                            *ws_idx,
                            *pane_id,
                        );
                        self.send_notify_to_foreground_client(
                            toast_notify_kind(self.app.state.toast_config.delivery)
                                .expect("toast forwarding requires a client notification kind"),
                            format!("{agent_label} {event_text}"),
                            non_empty_body(&context),
                        );
                    }
                }
            }

            // Forward sound notification when server-side sound policy allows it.
            // Clients still decide locally whether they can execute the side effect.
            if self.app.state.toast_config.delay_seconds == 0 && self.app.state.sound.allows(agent)
            {
                if let Some(sound) =
                    crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                        suppress_active_tab_notifications,
                        *prev_state,
                        new_state,
                        prev_agent_label.as_deref(),
                        agent_label.as_deref(),
                    )
                {
                    debug!(sound = ?sound, "forwarding sound notification from API request");
                    self.send_notify_to_foreground_client(
                        protocol::NotifyKind::Sound,
                        sound_notify_message(sound),
                        None,
                    );
                }
            }
        }

        if !skip_default_workspace && latest_shell_client(&self.clients).is_some() {
            changed |= self.app.ensure_default_workspace();
        }

        changed
    }

    /// Handle scheduled tasks for the headless server.
    ///
    /// Similar to the former App scheduler but without terminal resize polling.
    fn handle_scheduled_tasks_headless(&mut self, now: Instant, geometry_dirty: bool) -> bool {
        let mut changed = false;

        // No resize polling needed — server has no terminal.
        // Client resize messages drive size changes instead.

        if self
            .app
            .config_diagnostic_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.config_diagnostic_deadline = None;
            self.app.state.config_diagnostic = None;
            changed = true;
        }

        if self
            .app
            .toast_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.toast_deadline = None;
            self.app.state.toast = None;
            changed = true;
        }

        if self
            .app
            .state
            .next_pending_agent_notification_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let previous_toast = self.app.state.toast.clone();
            let mut deliveries = self.app.state.drain_due_agent_notifications(now);
            if !deliveries.is_empty() {
                self.app
                    .refresh_agent_notification_delivery_contexts(&mut deliveries);
                self.app.sync_toast_deadline(previous_toast);
                for delivery in &deliveries {
                    self.forward_agent_notification_delivery(delivery);
                }
                changed = true;
            }
        }

        if self.has_app_client() {
            self.app.start_git_status_refresh_if_due(now);
        }

        if self
            .app
            .next_auto_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.run_auto_update_check();
        }

        if self
            .app
            .next_agent_manifest_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.run_agent_manifest_update_check();
        }

        if self
            .app
            .session_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.start_background_session_save();
        }

        if let Some(deadline) = self
            .app
            .agent_metadata_deadline
            .filter(|deadline| now >= *deadline)
        {
            self.app.expire_metadata_at(deadline, now);
            changed = true;
        }

        changed |= self.app.handle_tab_bar_status_tasks(now);

        if geometry_dirty {
            self.app.pending_agent_resume_deadline = None;
        } else {
            self.app.sync_pending_agent_resume_deadline(now);
            changed |= self
                .app
                .start_pending_agent_resumes(self.app.pending_agent_resume_due(now));
        }
        changed
    }
}

fn client_pane_input_releases_press(event: &protocol::ClientPaneInputEvent) -> bool {
    matches!(
        event,
        protocol::ClientPaneInputEvent::Key {
            kind: protocol::ClientKeyKind::Release,
            ..
        } | protocol::ClientPaneInputEvent::Mouse {
            kind: protocol::ClientMouseKind::Up(_),
            ..
        }
    )
}

fn client_pane_input_has_interaction(events: &[protocol::ClientPaneInputEvent]) -> bool {
    events
        .iter()
        .any(|event| !client_pane_input_releases_press(event))
}

impl Drop for HeadlessServer {
    fn drop(&mut self) {
        let staged_files = self
            .clients
            .drain()
            .flat_map(|(_, client)| client.staged_clipboard_files)
            .collect::<Vec<_>>();
        crate::server::clipboard_image::remove_files(staged_files);
        let _ = self.cleanup_sockets();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Installs a Ctrl+C handler that sets the should_quit flag and wakes up
/// the event loop by sending a QuitSignal on the server event channel.
fn ctrlc_handler(should_quit: Arc<AtomicBool>, server_event_tx: mpsc::Sender<ServerEvent>) {
    let _ = ctrlc::set_handler(move || {
        should_quit.store(true, Ordering::Release);
        // Wake up the event loop so the quit flag is checked promptly.
        let _ = server_event_tx.try_send(ServerEvent::QuitSignal);
    });
}

/// Sleep until a deadline, or return pending if none.
async fn sleep_until_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending().await,
    }
}

fn sanitize_notification_text(value: &str, max_chars: usize) -> Option<String> {
    let mut sanitized = String::new();
    let mut previous_space = false;
    for ch in value.chars() {
        let replacement = if ch == '\n' || ch == '\r' || ch == '\t' {
            Some(' ')
        } else if ch.is_control() {
            None
        } else {
            Some(ch)
        };
        let Some(ch) = replacement else {
            continue;
        };
        if ch.is_whitespace() {
            if previous_space {
                continue;
            }
            previous_space = true;
            sanitized.push(' ');
        } else {
            previous_space = false;
            sanitized.push(ch);
        }
        if sanitized.chars().count() >= max_chars {
            break;
        }
    }
    let sanitized = sanitized.trim().to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn server_config_diagnostic_summaries(diagnostics: &[String]) -> (Option<String>, Option<String>) {
    (
        config::config_diagnostic_summary(diagnostics),
        config::config_diagnostic_summary_without_keybindings(diagnostics),
    )
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
