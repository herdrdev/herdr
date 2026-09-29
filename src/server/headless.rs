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
use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseEventKind};
use interprocess::local_socket::traits::Listener as _;
#[cfg(windows)]
use interprocess::local_socket::traits::Stream as _;
#[cfg(unix)]
use interprocess::local_socket::ListenerNonblockingMode;
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use base64::Engine;
use bytes::Bytes;

use crate::api;
use crate::app;
use crate::config;
use crate::events::AppEvent;
use crate::ipc::{
    bind_local_listener, remove_socket_file_if_owned, socket_file_identity, LocalListener,
    SocketFileIdentity,
};
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, FrameData, ServerMessage, MAX_FRAME_SIZE,
};
#[cfg(unix)]
use crate::server::client_accept::{
    accept_pending_client_connections, reject_pending_client_connections,
};
use crate::server::client_transport::ServerEvent;
use crate::server::clients::{
    events_include_interaction, latest_app_client, render_targets, terminal_stream_client_ids,
    ClientConnection, ClientConnectionMode, ClientTerminalGeometry, DeferredRender,
};
use crate::server::keybindings::{app_keybindings, apply_keybindings};
use crate::server::notifications::{
    should_forward_toast_to_clients, toast_message_from_state_change, toast_notify_kind,
};
use crate::server::socket_paths::{
    client_socket_path, prepare_socket_path, restrict_socket_permissions,
};
use crate::server::terminal_attach::paste_payload_for_runtime;

mod idle_render;
mod pane_graphics;

use crate::protocol::MAX_GRAPHICS_FRAME_SIZE;
use pane_graphics::RetainedGraphicsOutcome;

#[cfg(test)]
use crate::protocol::RenderEncoding;
#[cfg(test)]
use crate::server::client_transport::ClientWriter;
#[cfg(test)]
use std::fs;

const LIVE_HANDOFF_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(6);

fn wait_for_live_handoff_response_write(
    response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
) {
    let Some(response_write_complete) = response_write_complete else {
        return;
    };

    match response_write_complete.recv_timeout(LIVE_HANDOFF_RESPONSE_WRITE_TIMEOUT) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            warn!("timed out waiting for live handoff response write; old server exiting");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            warn!("live handoff response writer disconnected; old server exiting");
        }
    }
}

fn live_handoff_api_response(id: String, handoff_result: io::Result<()>) -> (String, bool) {
    let (response, succeeded) = match handoff_result {
        Ok(()) => (
            serde_json::to_string(&api::schema::SuccessResponse {
                id,
                result: api::schema::ResponseResult::Ok {},
            }),
            true,
        ),
        Err(err) => {
            warn!(error = %err, "live handoff failed");
            (
                serde_json::to_string(&api::schema::ErrorResponse {
                    id,
                    error: api::schema::ErrorBody {
                        code: "handoff_failed".into(),
                        message: err.to_string(),
                    },
                }),
                false,
            )
        }
    };
    (response.unwrap_or_else(|_| "{}".to_string()), succeeded)
}

fn sound_notify_message(sound: crate::sound::Sound) -> &'static str {
    match sound {
        crate::sound::Sound::Done => "agent done",
        crate::sound::Sound::Request => "agent attention",
    }
}

fn notification_show_response_shown(response: &str) -> bool {
    let Ok(response) = serde_json::from_str::<api::schema::SuccessResponse>(response) else {
        return false;
    };
    matches!(
        response.result,
        api::schema::ResponseResult::NotificationShow { shown: true, .. }
    )
}

fn work_item_detail_request(
    client: &ClientConnection,
) -> Option<(
    crate::app::state::DockHomeSection,
    Option<crate::app::state::WorkItemKey>,
    bool,
)> {
    if !client.is_full_app_client() {
        return None;
    }
    if let Some(view) = client.work_view.as_ref() {
        let selection = view.selected.clone().or_else(|| {
            view.snapshot.as_ref()?.items.iter().find_map(|item| {
                let number = item.pr_number?;
                Some(crate::app::state::WorkItemKey {
                    repo: item.repo.clone(),
                    pr_number: Some(number),
                    pr_url: item.pr_url.clone(),
                    ticket_id: None,
                })
            })
        });
        return Some((crate::app::state::DockHomeSection::Prs, selection, true));
    }
    let presentation = &client.dock_presentation;
    if !presentation.collapsed && presentation.tab == Some(crate::app::DockSurface::Pr) {
        let selection = presentation
            .active_tab_index
            .or_else(|| {
                presentation
                    .open_surfaces
                    .iter()
                    .position(|surface| *surface == crate::app::DockSurface::Pr)
            })
            .and_then(|index| presentation.tab_bindings.get(index))
            .and_then(Option::as_ref)
            .map(|binding| &binding.object)
            .filter(|object| object.surface == crate::app::DockSurface::Pr)
            .or_else(|| {
                presentation
                    .context_objects
                    .iter()
                    .find(|object| object.surface == crate::app::DockSurface::Pr)
            })
            .and_then(|object| {
                let repo = crate::work_context::repo_slug_from_pr_url(&object.key)?;
                let number = object
                    .key
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()?
                    .parse()
                    .ok()?;
                Some(crate::app::state::WorkItemKey {
                    repo,
                    pr_number: Some(number),
                    pr_url: Some(object.key.clone()),
                    ticket_id: None,
                })
            });
        return Some((crate::app::state::DockHomeSection::Prs, selection, true));
    }
    // A collapsed dock still renders a sidebar-selected ticket into the pane
    // area (`ui::dock::render_object_preview`), so that host needs its detail
    // too.
    if presentation.collapsed {
        if let Some(object) = presentation
            .object_preview
            .as_ref()
            .filter(|object| object.surface == crate::app::DockSurface::Linear)
        {
            return Some((
                crate::app::state::DockHomeSection::Tickets,
                Some(crate::app::state::WorkItemKey {
                    repo: String::new(),
                    pr_number: None,
                    pr_url: None,
                    ticket_id: Some(object.key.clone()),
                }),
                true,
            ));
        }
    }
    // The Linear surface owns a ticket detail exactly the way the pull-request
    // surface owns a PR detail. Without this branch the request fell through to
    // the home block, which reports the detail as hidden, so a server-backed
    // client showed a ticket header with no description, criteria, or comments.
    if !presentation.collapsed && presentation.tab == Some(crate::app::DockSurface::Linear) {
        let selection = presentation
            .active_tab_index
            .or_else(|| {
                presentation
                    .open_surfaces
                    .iter()
                    .position(|surface| *surface == crate::app::DockSurface::Linear)
            })
            .and_then(|index| presentation.tab_bindings.get(index))
            .and_then(Option::as_ref)
            .map(|binding| &binding.object)
            .filter(|object| object.surface == crate::app::DockSurface::Linear)
            .or_else(|| {
                presentation
                    .context_objects
                    .iter()
                    .find(|object| object.surface == crate::app::DockSurface::Linear)
            })
            // Keyed exactly as `dock::linear::focused_ticket_key` keys it, so the
            // fetched detail lands in the entry the render reads.
            .map(|object| crate::app::state::WorkItemKey {
                repo: String::new(),
                pr_number: None,
                pr_url: None,
                ticket_id: Some(object.key.clone()),
            });
        return Some((crate::app::state::DockHomeSection::Tickets, selection, true));
    }
    let selection = match presentation.home_section {
        crate::app::state::DockHomeSection::Prs => presentation.home_selection.clone(),
        crate::app::state::DockHomeSection::Tickets => presentation.home_ticket_selection.clone(),
        crate::app::state::DockHomeSection::XPolls => presentation.home_poll_selection.clone(),
    };
    Some((
        presentation.home_section,
        selection,
        !presentation.collapsed && presentation.tab == Some(crate::app::DockSurface::Home),
    ))
}

fn alt_screen_restore_error_response(id: String) -> String {
    serde_json::to_string(&api::schema::ErrorResponse {
        id,
        error: api::schema::ErrorBody {
            code: "alternate_screen_restore_failed".into(),
            message: "pane read cancelled an alternate-screen history capture, but the live viewport could not be restored; retry the read".into(),
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"alternate_screen_restore_failed","message":"alternate-screen history capture could not restore the live viewport"}}"#.to_owned()
    })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PtyRenderState {
    Clean,
    Hidden,
    Visible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RetainedRenderInput {
    needs_full_render: bool,
    needs_graphics_render: bool,
    pty: PtyRenderState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetainedRenderPlan {
    Full,
    Graphics,
    Pty,
    HiddenPty,
}

fn retained_render_plan(input: RetainedRenderInput) -> RetainedRenderPlan {
    if input.needs_full_render {
        RetainedRenderPlan::Full
    } else if input.needs_graphics_render && input.pty != PtyRenderState::Visible {
        RetainedRenderPlan::Graphics
    } else {
        match input.pty {
            PtyRenderState::Visible => RetainedRenderPlan::Pty,
            PtyRenderState::Hidden => RetainedRenderPlan::HiddenPty,
            PtyRenderState::Clean => RetainedRenderPlan::Full,
        }
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

fn rect_fits_frame(rect: Rect, frame: &FrameData) -> bool {
    rect.x.saturating_add(rect.width) <= frame.width
        && rect.y.saturating_add(rect.height) <= frame.height
}

fn apply_terminal_dirty_patch(
    frame: &mut FrameData,
    area: Rect,
    patch: crate::pane::TerminalDirtyPatch,
) -> bool {
    if !rect_fits_frame(area, frame) {
        return false;
    }
    let width = usize::from(frame.width);
    for (local_y, row_cells) in patch.rows {
        if local_y >= area.height || row_cells.len() != usize::from(area.width) {
            return false;
        }
        let frame_y = area.y + local_y;
        let start = usize::from(frame_y) * width + usize::from(area.x);
        let end = start + usize::from(area.width);
        if end > frame.cells.len() {
            return false;
        }
        frame.cells[start..end].clone_from_slice(&row_cells);
    }
    true
}

fn dirty_patch_intersects_hyperlinks(
    frame: &FrameData,
    area: Rect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> bool {
    if frame.hyperlinks.is_empty() || !rect_fits_frame(area, frame) {
        return false;
    }
    let width = usize::from(frame.width);
    for (local_y, _) in &patch.rows {
        if *local_y >= area.height {
            return true;
        }
        let frame_y = area.y + *local_y;
        let start = usize::from(frame_y) * width + usize::from(area.x);
        let end = start + usize::from(area.width);
        if end > frame.cells.len() {
            return true;
        }
        if frame.cells[start..end]
            .iter()
            .any(|cell| cell.hyperlink.is_some())
        {
            return true;
        }
    }
    false
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
}

enum AltScreenReadConflict {
    None,
    Cancelled,
    Defer,
    RestoreFailed,
}

#[derive(Clone, Copy, Debug, Default)]
struct ScheduledTaskRender {
    changed: bool,
    sidebar_animation_only: bool,
}

/// Complete attach-local app presentation installed while one client's input
/// is resolved. Cell and pixel input share this transaction so hit geometry
/// cannot be computed from another client's sidebar, dock, or detail state.
struct ClientInputPresentation {
    sidebar: crate::app::state::SidebarPresentationState,
    dock: crate::app::state::DockPresentationState,
    notepad: crate::notepad::NotepadPresentationState,
    loop_run_history_detail: Option<crate::app::state::LoopRunHistoryDetail>,
    aloop_run_detail: Option<crate::app::state::AloopRunDetail>,
    symphony_detail: Option<crate::app::state::SymphonyDetail>,
    work_view: Option<crate::app::state::WorkViewState>,
    usage_view: Option<crate::app::state::UsageViewState>,
}

impl ClientInputPresentation {
    fn take(client: &mut ClientConnection) -> Self {
        Self {
            sidebar: std::mem::take(&mut client.sidebar_presentation),
            dock: std::mem::take(&mut client.dock_presentation),
            notepad: std::mem::take(&mut client.notepad_presentation),
            loop_run_history_detail: client.loop_run_history_detail.take(),
            aloop_run_detail: client.aloop_run_detail.take(),
            symphony_detail: client.symphony_detail.take(),
            work_view: client.work_view.take(),
            usage_view: client.usage_view.take(),
        }
    }

    fn install(&mut self, state: &mut crate::app::state::AppState) {
        state.swap_sidebar_presentation(&mut self.sidebar);
        state.reconcile_sidebar_presentation();
        state.swap_dock_presentation(&mut self.dock);
        state.reconcile_dock_home_with_focused_pane();
        state.notepad.swap_presentation(&mut self.notepad);
        state.swap_loop_run_history_detail(&mut self.loop_run_history_detail);
        state.swap_aloop_run_detail(&mut self.aloop_run_detail);
        state.swap_symphony_detail(&mut self.symphony_detail);
        state.swap_work_view(&mut self.work_view);
        state.swap_usage_view(&mut self.usage_view);
    }

    fn uninstall(&mut self, state: &mut crate::app::state::AppState) {
        state.swap_usage_view(&mut self.usage_view);
        state.swap_work_view(&mut self.work_view);
        state.swap_symphony_detail(&mut self.symphony_detail);
        state.swap_aloop_run_detail(&mut self.aloop_run_detail);
        state.swap_loop_run_history_detail(&mut self.loop_run_history_detail);
        state.swap_sidebar_presentation(&mut self.sidebar);
        state.swap_dock_presentation(&mut self.dock);
        state.notepad.swap_presentation(&mut self.notepad);
    }

    fn store(self, client: &mut ClientConnection) {
        client.sidebar_presentation = self.sidebar;
        client.dock_presentation = self.dock;
        client.notepad_presentation = self.notepad;
        client.loop_run_history_detail = self.loop_run_history_detail;
        client.aloop_run_detail = self.aloop_run_detail;
        client.symphony_detail = self.symphony_detail;
        client.work_view = self.work_view;
        client.usage_view = self.usage_view;
    }
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
    /// Last instant a full TUI client was attached. Timer work-index refreshes
    /// stop after six intervals without a viewer and resume on the next attach.
    last_app_client_seen: Instant,
    #[cfg(unix)]
    next_client_id: u64,
    /// The client currently driving the shared pane runtime size, theme, and input keybindings.
    foreground_client_id: Option<u64>,
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

fn apply_terminal_attach_scroll(
    runtime: &crate::terminal::TerminalRuntime,
    source: AttachScrollSource,
    direction: AttachScrollDirection,
    lines: u16,
    column: Option<u16>,
    row: Option<u16>,
    modifiers: u8,
) -> Result<(), String> {
    let wheel_kind = match direction {
        AttachScrollDirection::Up => MouseEventKind::ScrollUp,
        AttachScrollDirection::Down => MouseEventKind::ScrollDown,
    };
    if let AttachScrollSource::PageKey { input } = source {
        let host_scroll = runtime
            .plain_page_keys_use_host_scrollback()
            .unwrap_or(false);
        if host_scroll {
            match direction {
                AttachScrollDirection::Up => runtime.scroll_up(lines.max(1) as usize),
                AttachScrollDirection::Down => runtime.scroll_down(lines.max(1) as usize),
            }
            return Ok(());
        }
        return apply_terminal_attach_input(runtime, input);
    }

    match runtime.wheel_routing() {
        Some(crate::pane::WheelRouting::MouseReport) => {
            runtime.scroll_reset();
            let position = crate::input::mouse::Position::Cell {
                column: column.unwrap_or(0),
                row: row.unwrap_or(0),
            };
            let Some(bytes) = runtime.encode_mouse_wheel(
                wheel_kind,
                position,
                KeyModifiers::from_bits_truncate(modifiers),
            ) else {
                return Err(format!(
                    "failed to encode terminal attach mouse wheel event: {wheel_kind:?}"
                ));
            };
            runtime
                .try_send_bytes(Bytes::from(bytes))
                .map_err(|err| format!("terminal attach mouse wheel input failed: {err}"))?;
        }
        Some(crate::pane::WheelRouting::AlternateScroll) => {
            runtime.scroll_reset();
            let Some(bytes) = runtime.encode_alternate_scroll(wheel_kind) else {
                return Ok(());
            };
            runtime
                .try_send_bytes(Bytes::from(bytes))
                .map_err(|err| format!("terminal attach alternate scroll input failed: {err}"))?;
        }
        Some(crate::pane::WheelRouting::HostScroll) | None => match direction {
            AttachScrollDirection::Up => runtime.scroll_up(lines.max(1) as usize),
            AttachScrollDirection::Down => runtime.scroll_down(lines.max(1) as usize),
        },
    }
    Ok(())
}

fn apply_terminal_attach_input(
    runtime: &crate::terminal::TerminalRuntime,
    data: Vec<u8>,
) -> Result<(), String> {
    runtime.scroll_reset();
    if let Some(text) = crate::raw_input::complete_text_bracketed_paste(&data) {
        runtime
            .try_send_paste(text.to_owned())
            .map_err(|err| format!("terminal attach paste failed: {err}"))
    } else {
        runtime
            .try_send_bytes(Bytes::from(data))
            .map_err(|err| format!("terminal attach input failed: {err}"))
    }
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

        let listener = bind_local_listener(&client_path)?;
        restrict_socket_permissions(&client_path)?;
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
            last_app_client_seen: Instant::now(),
            #[cfg(unix)]
            next_client_id: 1,
            foreground_client_id: None,
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
    /// This is the server's main loop — analogous to `App::run()` but without
    /// a real terminal. It:
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

        // No input_rx needed — server doesn't read stdin.
        // We use None for input_rx so the event loop doesn't try to read from stdin.
        self.app.input_rx = None;

        let mut needs_render = true;
        let mut needs_full_render = true;
        let mut needs_graphics_render = false;
        let mut needs_sidebar_animation_render = false;

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
            let terminal_title_change = self.app.sync_pending_terminal_titles();
            if terminal_title_change.chrome_changed
                || (terminal_title_change.raw_changed
                    && self.app.terminal_title_sidebar_configured())
            {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.terminal_title");
            }

            // 2. Drain a bounded internal-event batch. API handlers perform an
            // exhaustive forwarding-aware drain before reading pane/runtime state.
            if self.drain_internal_events_with_forwarding() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
                crate::render_prof::event("full_render_cause.internal_events");
            }
            if self.app.reconcile_remote_focus_lifecycle() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
                crate::render_prof::event("full_render_cause.remote_focus_lifecycle");
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
            let render_was_already_pending =
                needs_render || needs_full_render || needs_graphics_render;
            let scheduled_render =
                self.handle_scheduled_tasks_headless_with_render_kind(now, needs_render);
            if scheduled_render.changed {
                needs_render = true;
                if scheduled_render.sidebar_animation_only && !render_was_already_pending {
                    needs_sidebar_animation_render = true;
                    crate::render_prof::event("animation_render.request");
                } else {
                    needs_full_render = true;
                    needs_graphics_render = false;
                    needs_sidebar_animation_render = false;
                    crate::render_prof::event("full_render_cause.scheduled_tasks");
                }
            }

            if self.handle_deferred_requests_headless() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
            }

            self.poll_pending_alt_screen_reads(now);
            if self.process_deferred_alt_screen_reads() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
            }

            if latest_app_client(&self.clients).is_some() && self.app.ensure_default_workspace() {
                needs_render = true;
                needs_full_render = true;
                needs_graphics_render = false;
                crate::render_prof::event("full_render_cause.default_workspace");
            }

            if self.app.pane_graphics.retain_live_panes(&self.app.state) {
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
            self.stream_host_keyboard_enhancement_flags();

            // 7. Render virtually and stream frames.
            let render_cadence_due = self.app.can_render_now(now);
            if needs_render
                && (render_cadence_due
                    || (self.app.can_present_now(now)
                        && self.has_pending_presentation_work(
                            needs_full_render,
                            needs_graphics_render,
                        )))
            {
                if self.app.sync_status_context_before_render() {
                    needs_full_render = true;
                    needs_graphics_render = false;
                }
                let mut staged_render_request = None;
                if needs_sidebar_animation_render && !needs_full_render && !needs_graphics_render {
                    if self.app.render_dirty.is_pending() {
                        let request = self.app.render_dirty.take();
                        let visible_pty =
                            self.pty_sources_visible_to_any_render_target(&request.pty_sources);
                        // Hidden PTY output keeps runtime state current but does not invalidate
                        // the frame any attached client can see.
                        if request.generic
                            || !request.terminal_title_sources.is_empty()
                            || visible_pty
                        {
                            staged_render_request = Some(request);
                            needs_full_render = true;
                        }
                    }
                    if !needs_full_render {
                        if self.render_sidebar_animation_and_stream() {
                            self.app.record_render_attempt(now, true);
                            needs_render = false;
                            needs_sidebar_animation_render = false;
                            continue;
                        }
                        needs_full_render = true;
                    }
                }
                crate::render_prof::event("render.attempt");
                let render_request =
                    staged_render_request.unwrap_or_else(|| self.app.render_dirty.take());
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
                if needs_full_render {
                    crate::render_prof::event("retained_gate.needs_full_render");
                } else if !pty_dirty {
                    crate::render_prof::event("retained_gate.not_pty_dirty");
                }
                let pty = if !pty_dirty {
                    PtyRenderState::Clean
                } else if self.pty_sources_visible_to_any_render_target(&render_request.pty_sources)
                {
                    PtyRenderState::Visible
                } else {
                    PtyRenderState::Hidden
                };
                let mut deferred_graphics = false;
                let render_plan = retained_render_plan(RetainedRenderInput {
                    needs_full_render,
                    needs_graphics_render,
                    pty,
                });
                let rendered_retained = match render_plan {
                    RetainedRenderPlan::Full => false,
                    RetainedRenderPlan::Graphics => {
                        match self.render_retained_graphics_update_and_stream() {
                            RetainedGraphicsOutcome::Sent => true,
                            RetainedGraphicsOutcome::Deferred => {
                                deferred_graphics = true;
                                false
                            }
                            RetainedGraphicsOutcome::Fallback => false,
                        }
                    }
                    RetainedRenderPlan::Pty => self.render_retained_pty_update_and_stream(),
                    RetainedRenderPlan::HiddenPty => {
                        crate::render_prof::event("render.skipped.hidden_sources");
                        true
                    }
                };
                if deferred_graphics {
                    needs_render = false;
                    continue;
                }
                if !rendered_retained {
                    crate::render_prof::event("full_render.invoke");
                    self.render_and_stream();
                }
                self.app
                    .record_render_attempt(now, render_plan != RetainedRenderPlan::HiddenPty);
                needs_render = false;
                needs_full_render = false;
                needs_graphics_render = false;
                needs_sidebar_animation_render = false;
                continue;
            }

            // 8. Wait for next event.
            let next_deadline = self
                .app
                .next_headless_loop_deadline_with_client_refresh(
                    now,
                    needs_render,
                    self.has_app_client(),
                )
                .map(|deadline| deadline.min(now + CLIENT_ACCEPT_POLL_INTERVAL))
                .or(Some(now + CLIENT_ACCEPT_POLL_INTERVAL));
            let next_deadline = self.pomodoro_animation_deadline(now).into_iter().fold(
                next_deadline,
                |deadline, pomodoro| {
                    Some(deadline.map_or(pomodoro, |current| current.min(pomodoro)))
                },
            );
            let next_deadline = self
                .pending_alt_screen_reads
                .iter()
                .map(|pending| pending.next_deadline())
                .fold(next_deadline, |deadline, pending| {
                    Some(deadline.map_or(pending, |current| current.min(pending)))
                });
            let next_deadline = self
                .clients
                .values()
                .filter_map(|client| client.dock_presentation.hover_tooltip_deadline())
                .fold(next_deadline, |deadline, hover| {
                    Some(deadline.map_or(hover, |current| current.min(hover)))
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
                    LoopEvent::ServerEvent(ServerEvent::ClientConnected { writer, .. }) => {
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
        if !self.app.no_session {
            self.app.save_session_on_shutdown();
        }

        info!("headless server exiting");
        Ok(())
    }

    fn handle_deferred_requests_headless(&mut self) -> bool {
        let mut needs_render = false;

        if self.app.state.request_complete_onboarding {
            self.app.state.request_complete_onboarding = false;
            self.app.open_settings_from_onboarding();
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_onboarding");
        }

        if self.app.state.request_new_workspace {
            self.app.state.request_new_workspace = false;
            let response = self.headless_workspace_create("headless.workspace.create", None, None);
            if let Err(error) = response {
                error!(
                    code = %error.code,
                    message = %error.message,
                    "failed to create workspace"
                );
            }
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_new_workspace");
        }

        if self.app.apply_pane_toggle_request() {
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_pane_toggle");
        }

        if self.app.apply_git_action_request() {
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_git_action");
        }
        if self.app.apply_notepad_request() {
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_notepad");
        }
        if self.app.apply_add_project_clone_request() {
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_add_project_clone");
        }
        if self.app.apply_user_action_request() {
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_user_action");
        }
        if self.app.apply_save_add_action_request() {
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_add_action_save");
        }

        if self.app.state.request_new_tab {
            self.app.state.request_new_tab = false;
            let label = self.app.state.requested_new_tab_name.take();
            let response = self.headless_tab_create("headless.tab.create", label);
            if let Err(error) = response {
                error!(
                    code = %error.code,
                    message = %error.message,
                    "failed to create tab"
                );
            }
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_new_tab");
        }

        needs_render |= self.handle_client_overlay_deferred_requests();

        if let Some(cwd) = self.app.state.request_new_workspace_cwd.take() {
            let response = self.headless_workspace_create(
                "headless.workspace.create_cwd",
                Some(cwd.display().to_string()),
                None,
            );
            if let Err(error) = response {
                error!(
                    code = %error.code,
                    message = %error.message,
                    "failed to create workspace at requested cwd"
                );
                self.app.state.set_server_mode(app::Mode::Navigate);
            }
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_workspace_cwd");
        }

        if self.app.state.request_reload_config {
            self.app.state.request_reload_config = false;
            self.reload_server_config(true);
            needs_render = true;
            crate::render_prof::event("full_render_cause.config_reload");
        }

        needs_render
    }

    fn handle_client_overlay_deferred_requests(&mut self) -> bool {
        let mut changed = false;
        if let Some(ws_idx) = self.app.state.request_new_linked_worktree.take() {
            self.app.open_new_linked_worktree_dialog(ws_idx);
            changed = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_dialog");
        }
        if let Some(ws_idx) = self.app.state.request_open_existing_worktree.take() {
            self.app.open_existing_worktree_dialog(ws_idx);
            changed = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_dialog");
        }
        if let Some(ws_idx) = self.app.state.request_remove_linked_worktree.take() {
            self.app.open_remove_linked_worktree_confirmation(ws_idx);
            changed = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_dialog");
        }
        if self.app.state.request_submit_worktree_create {
            self.app.state.request_submit_worktree_create = false;
            self.app.submit_worktree_create_via_api();
            changed = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_submit");
        }
        if self.app.state.request_submit_worktree_open {
            self.app.state.request_submit_worktree_open = false;
            self.app.submit_worktree_open_via_api();
            changed = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_submit");
        }
        if self.app.state.request_submit_worktree_remove {
            self.app.state.request_submit_worktree_remove = false;
            self.app.submit_worktree_remove_via_api();
            changed = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_submit");
        }
        changed
    }

    fn headless_workspace_create(
        &mut self,
        id: &'static str,
        cwd: Option<String>,
        label: Option<String>,
    ) -> Result<(), api::schema::ErrorBody> {
        self.dispatch_headless_runtime_mutation(
            id,
            api::schema::Method::WorkspaceCreate(api::schema::WorkspaceCreateParams {
                cwd,
                focus: true,
                label,
                env: Default::default(),
                work_context: None,
                source_workspace_id: None,
            }),
        )
    }

    fn headless_tab_create(
        &mut self,
        id: &'static str,
        label: Option<String>,
    ) -> Result<(), api::schema::ErrorBody> {
        self.dispatch_headless_runtime_mutation(
            id,
            api::schema::Method::TabCreate(api::schema::TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: true,
                label,
                env: Default::default(),
                work_context: None,
            }),
        )
    }

    fn dispatch_headless_runtime_mutation(
        &mut self,
        id: &'static str,
        method: api::schema::Method,
    ) -> Result<(), api::schema::ErrorBody> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        self.handle_api_request_with_shutdown_check_inner(
            api::ApiRequestMessage {
                request: api::schema::Request {
                    id: id.to_string(),
                    method,
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            },
            true,
            false,
        );
        match response_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(response) => serde_json::from_str::<api::schema::ErrorResponse>(&response)
                .map(|response| Err(response.error))
                .unwrap_or(Ok(())),
            Err(err) => Err(api::schema::ErrorBody {
                code: "internal_error".into(),
                message: format!("headless runtime mutation response failed: {err}"),
            }),
        }
    }

    fn allocate_activity_stamp(&mut self) -> u64 {
        let stamp = self.next_activity_stamp;
        self.next_activity_stamp = self.next_activity_stamp.saturating_add(1);
        stamp
    }

    fn resize_shared_runtime_to_effective_size(&mut self) {
        self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(true);
    }

    fn resize_shared_runtime_to_effective_size_before_input(&mut self) {
        self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(false);
    }

    fn current_client_terminal_geometries(
        &self,
        client_id: u64,
    ) -> HashMap<crate::terminal::TerminalId, ClientTerminalGeometry> {
        let Some(client) = self
            .clients
            .get(&client_id)
            .filter(|client| client.is_full_app_client())
        else {
            return HashMap::new();
        };
        let Some(workspace) = self
            .app
            .state
            .active
            .and_then(|index| self.app.state.workspaces.get(index))
        else {
            return HashMap::new();
        };

        let mut geometries = self
            .app
            .state
            .view
            .pane_infos
            .iter()
            .filter(|info| info.inner_rect.width > 0 && info.inner_rect.height > 0)
            .filter_map(|info| {
                workspace.terminal_id(info.id).cloned().map(|terminal_id| {
                    (
                        terminal_id,
                        ClientTerminalGeometry {
                            cols: info.inner_rect.width,
                            rows: info.inner_rect.height,
                            cell_size: client.cell_size,
                        },
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        if let (Some(popup), Some((_outer, inner))) = (
            self.app.state.popup_pane.as_ref(),
            crate::ui::popup_pane_rects(&self.app.state, self.app.state.view.terminal_area),
        ) {
            if inner.width > 0 && inner.height > 0 {
                geometries.insert(
                    popup.terminal_id.clone(),
                    ClientTerminalGeometry {
                        cols: inner.width,
                        rows: inner.height,
                        cell_size: client.cell_size,
                    },
                );
            }
        }
        geometries
    }

    fn update_client_terminal_geometries(
        &mut self,
        client_id: u64,
    ) -> std::collections::HashSet<crate::terminal::TerminalId> {
        let next = self.current_client_terminal_geometries(client_id);
        let Some(client) = self.clients.get_mut(&client_id) else {
            return std::collections::HashSet::new();
        };
        if client.terminal_geometries == next {
            return std::collections::HashSet::new();
        }
        let affected = client
            .terminal_geometries
            .keys()
            .chain(next.keys())
            .cloned()
            .collect();
        client.terminal_geometries = next;
        affected
    }

    fn client_terminal_ids(
        &self,
        client_id: u64,
    ) -> std::collections::HashSet<crate::terminal::TerminalId> {
        let Some(client) = self.clients.get(&client_id) else {
            return std::collections::HashSet::new();
        };
        let mut terminal_ids = client
            .terminal_geometries
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        if let ClientConnectionMode::TerminalAttach { terminal_id, .. }
        | ClientConnectionMode::TerminalObserve { terminal_id } = &client.mode
        {
            if let Some(terminal_id) = self.terminal_id_by_string(terminal_id) {
                terminal_ids.insert(terminal_id);
            }
        }
        terminal_ids
    }

    fn reconcile_terminal_geometries(
        &self,
        terminal_ids: impl IntoIterator<Item = crate::terminal::TerminalId>,
    ) {
        for terminal_id in terminal_ids {
            let terminal_id_string = terminal_id.to_string();
            let mut cols = None::<u16>;
            let mut rows = None::<u16>;
            let mut cell_width_px = None::<u32>;
            let mut cell_height_px = None::<u32>;

            for client in self.clients.values() {
                let geometry = match &client.mode {
                    ClientConnectionMode::App => {
                        client.terminal_geometries.get(&terminal_id).copied()
                    }
                    ClientConnectionMode::TerminalAttach {
                        terminal_id: attached,
                        ..
                    }
                    | ClientConnectionMode::TerminalObserve {
                        terminal_id: attached,
                    } if attached == &terminal_id_string => Some(ClientTerminalGeometry {
                        cols: client.terminal_size.0,
                        rows: client.terminal_size.1,
                        cell_size: client.cell_size,
                    }),
                    ClientConnectionMode::TerminalAttach { .. }
                    | ClientConnectionMode::TerminalObserve { .. } => None,
                };
                let Some(geometry) = geometry else {
                    continue;
                };
                cols = Some(cols.map_or(geometry.cols, |current| current.min(geometry.cols)));
                rows = Some(rows.map_or(geometry.rows, |current| current.min(geometry.rows)));
                if geometry.cell_size.width_px > 0 {
                    cell_width_px = Some(
                        cell_width_px.map_or(geometry.cell_size.width_px, |current| {
                            current.min(geometry.cell_size.width_px)
                        }),
                    );
                }
                if geometry.cell_size.height_px > 0 {
                    cell_height_px = Some(
                        cell_height_px.map_or(geometry.cell_size.height_px, |current| {
                            current.min(geometry.cell_size.height_px)
                        }),
                    );
                }
            }

            let (Some(cols), Some(rows)) = (cols, rows) else {
                continue;
            };
            if let Some(runtime) = self.app.terminal_runtimes.get(&terminal_id) {
                runtime.resize(
                    rows,
                    cols,
                    cell_width_px.unwrap_or(0),
                    cell_height_px.unwrap_or(0),
                );
            }
        }
    }

    fn pomodoro_client_area(client: &ClientConnection) -> Rect {
        Rect::new(0, 0, client.terminal_size.0, client.terminal_size.1)
    }

    fn pomodoro_animation_deadline(&self, now: Instant) -> Option<Instant> {
        self.clients
            .values()
            .filter(|client| client.writer.is_some() && client.is_full_app_client())
            .filter_map(|client| {
                crate::ui::pomodoro::animation_deadline_at(
                    &self.app.state,
                    Self::pomodoro_client_area(client),
                    now,
                )
            })
            .min()
    }

    fn pomodoro_animation_due(&self, now: Instant) -> bool {
        self.clients
            .values()
            .filter(|client| client.writer.is_some() && client.is_full_app_client())
            .any(|client| {
                crate::ui::pomodoro::animation_due_at(
                    &self.app.state,
                    Self::pomodoro_client_area(client),
                    now,
                    self.app.last_render_at,
                )
            })
    }

    fn pomodoro_input_presentation_for_input(
        &self,
        source_id: u64,
    ) -> crate::ui::pomodoro::InputPresentation {
        self.clients
            .get(&source_id)
            .filter(|client| client.writer.is_some() && client.is_full_app_client())
            .map_or_else(Default::default, |client| {
                let mut presentation = client.pomodoro_presentation;
                if presentation.area.is_empty() {
                    // The first input can arrive before the first committed frame; in that case
                    // the stored input gate has no valid area yet, so derive one from terminal size.
                    presentation.area = Self::pomodoro_client_area(client);
                }
                crate::ui::pomodoro::input_gate_at(&self.app.state, presentation, Instant::now())
            })
    }

    fn resize_shared_runtime_to_effective_size_with_pending_agent_resumes(
        &mut self,
        start_pending_agent_resumes: bool,
    ) {
        if self.foreground_client_id.is_none() {
            return;
        }
        let Some(client_id) = self.foreground_client_id else {
            return;
        };
        let Some(cell_size) = self.clients.get(&client_id).map(|client| client.cell_size) else {
            return;
        };
        let (cols, rows) = self.effective_size;
        let area = Rect::new(0, 0, cols, rows);
        if cell_size.is_known() {
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
        let affected = self.update_client_terminal_geometries(client_id);
        self.reconcile_terminal_geometries(affected);
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

    fn sync_headless_view_geometry(&mut self) {
        crate::ui::compute_view_without_resizing_panes(
            &mut self.app.state,
            &self.app.terminal_runtimes,
            Rect::new(0, 0, self.headless_size.0, self.headless_size.1),
        );
    }

    fn sync_foreground_client_state(&mut self) {
        self.app.direct_graphics_available = self.direct_graphics_available();
        #[cfg(unix)]
        crate::kitty_graphics::set_direct_host_graphics(
            self.app.direct_graphics_available
                && self.foreground_client_id.is_some_and(|id| {
                    self.clients
                        .get(&id)
                        .is_some_and(|client| client.terminal_file_graphics)
                }),
        );
        #[cfg(not(unix))]
        crate::kitty_graphics::set_direct_host_graphics(false);
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
            self.sync_headless_view_geometry();
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
            self.sync_headless_view_geometry();
            let server_keybindings = self.server_keybindings.clone();
            apply_keybindings(&mut self.app, &server_keybindings);
            self.sync_visible_server_config_diagnostic(false);
            return;
        };

        let terminal_size = client.terminal_size;
        let outer_terminal_focus = client.outer_terminal_focus;
        let host_cell_size = if client.cell_size.is_known() {
            client.cell_size
        } else {
            crate::kitty_graphics::HostCellSize::default()
        };
        let host_terminal_theme = client.host_terminal_theme;
        let host_terminal_appearance = client.host_terminal_appearance;
        let host_terminal_appearance_explicit = client.host_terminal_appearance_explicit;
        let uses_local_keybindings = client.keybindings.is_some();
        let keybindings = client
            .keybindings
            .as_deref()
            .unwrap_or(&self.server_keybindings)
            .clone();

        self.effective_size = terminal_size;
        self.app.state.outer_terminal_focus = outer_terminal_focus;
        self.app.state.host_cell_size = host_cell_size;
        apply_keybindings(&mut self.app, &keybindings);
        self.sync_visible_server_config_diagnostic(uses_local_keybindings);
        if outer_terminal_focus == Some(true) {
            self.app.state.mark_active_pane_seen();
        }
        self.app.set_host_terminal_appearance_state(
            host_terminal_appearance,
            host_terminal_appearance_explicit,
        );
        self.app.set_host_terminal_theme(host_terminal_theme);
    }

    /// Client-routed completions already update the presentation swapped into
    /// `AppState`. External API focus has no originating client, so project its
    /// mode-neutral pane intent into every attached full-app presentation.
    fn finish_pending_client_pane_focus(&mut self) -> bool {
        if !self.app.take_pending_client_pane_focus() {
            return false;
        }
        if self.app.active_overlay_client_id.is_some() {
            return false;
        }
        let client_ids = self
            .clients
            .iter()
            .filter_map(|(client_id, client)| client.is_full_app_client().then_some(*client_id))
            .collect::<Vec<_>>();
        for client_id in &client_ids {
            if let Some(client) = self.clients.get_mut(client_id) {
                client.sidebar_presentation.focus_intent =
                    crate::app::state::ClientFocusIntent::Pane;
                client.request_repaint();
            }
            if let Some(policy) = self.client_presentation_policy(*client_id) {
                self.sync_client_input_source(*client_id, &policy);
            }
        }
        !client_ids.is_empty()
    }

    #[cfg(unix)]
    fn collect_handoff_panes(
        &self,
    ) -> HashMap<crate::terminal::TerminalId, (u32, bool, Option<std::time::Instant>)> {
        let mut panes = HashMap::new();
        for workspace in &self.app.state.workspaces {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    if self
                        .app
                        .terminal_runtimes
                        .get(&pane.attached_terminal_id)
                        .is_some_and(|runtime| runtime.is_remote_proxy())
                    {
                        continue;
                    }
                    panes.insert(
                        pane.attached_terminal_id.clone(),
                        (pane_id.raw(), pane.seen, pane.done_since),
                    );
                }
            }
        }
        panes
    }

    #[cfg(unix)]
    fn perform_live_handoff(
        &mut self,
        params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        info!("starting live handoff");
        let import_exe = params.import_exe.as_deref().map(std::path::PathBuf::from);
        let socket_path = crate::server::handoff::handoff_socket_path();
        let token = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let listener = match crate::server::handoff::bind_listener(&socket_path) {
            Ok(listener) => listener,
            Err(err) => {
                self.handoff_in_progress = false;
                return Err(err);
            }
        };

        let pane_by_terminal = self.collect_handoff_panes();
        let editor_terminals = self.app.dock_editor_handoff_terminals();
        if pane_by_terminal.len() + editor_terminals.len()
            > crate::server::handoff::MAX_FDS_PER_HANDOFF
        {
            let _ = std::fs::remove_file(&socket_path);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "live handoff supports at most {} panes in one update; close panes or restart herdr normally",
                    crate::server::handoff::MAX_FDS_PER_HANDOFF
                ),
            ));
        }

        self.handoff_in_progress = true;
        self.disconnect_all_clients_for_handoff();
        let _ = reject_pending_client_connections(&self.client_listener);

        let mut paused_terminal_ids = Vec::new();
        for terminal_id in pane_by_terminal.keys().chain(
            editor_terminals
                .iter()
                .map(|(terminal_id, _, _)| terminal_id),
        ) {
            if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                if let Err(err) = runtime.pause_handoff_reader(Duration::from_secs(2)) {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                    return Err(err);
                }
                paused_terminal_ids.push(terminal_id.clone());
            }
        }

        let snapshot = crate::persist::capture(
            &self.app.state.workspaces,
            &self.app.state.terminals,
            &self.app.terminal_runtimes,
            self.app.state.active,
            self.app.state.selected,
            self.app.state.sidebar_width,
            self.app.state.sidebar_section_split,
            self.app.state.collapsed_space_keys.clone(),
            self.app.state.prio_panel_collapsed,
            self.app.state.window_cycle_mode,
            self.app.state.skip_collapsed_cycle,
        );

        let mut handoff_entries = Vec::new();
        let handoff_captured_at = Instant::now();
        for (terminal_id, runtime) in self.app.terminal_runtimes.iter() {
            let Some((pane_id, pane_seen, pane_done_since)) =
                pane_by_terminal.get(terminal_id).copied()
            else {
                continue;
            };
            let mut handoff_runtime = runtime.handoff_runtime_state(pane_id);
            let terminal = self.app.state.terminals.get(terminal_id);
            handoff_runtime.agent_activity = terminal
                .and_then(|terminal| terminal.agent_activity_handoff_state(handoff_captured_at));
            handoff_runtime.agent_state = terminal
                .and_then(|terminal| terminal.terminal_agent_handoff_state(handoff_captured_at));
            handoff_runtime.stall_nudge = self.app.stall_nudge_handoff_state(
                crate::layout::PaneId::from_raw(pane_id),
                terminal_id,
                handoff_captured_at,
            );
            handoff_runtime.human_draft = self
                .app
                .human_draft_handoff_state(crate::layout::PaneId::from_raw(pane_id));
            handoff_runtime.pane_seen = Some(pane_seen);
            handoff_runtime.pane_done_for_ms = pane_done_since.map(|done_since| {
                handoff_captured_at
                    .saturating_duration_since(done_since)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            });
            let has_agent_session =
                terminal.is_some_and(|terminal| terminal.persisted_agent_session.is_some());
            if !has_agent_session {
                handoff_runtime.initial_history_ansi = runtime.handoff_history_ansi();
            }
            handoff_entries.push((terminal_id.clone(), handoff_runtime));
        }

        let mut dock_editors = Vec::new();
        for (terminal_id, agent_pane_id, editor_pane_id) in &editor_terminals {
            let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) else {
                continue;
            };
            handoff_entries.push((
                terminal_id.clone(),
                runtime.handoff_runtime_state(editor_pane_id.raw()),
            ));
            dock_editors.push(crate::server::handoff::DockEditorHandoff {
                agent_pane_id: agent_pane_id.raw(),
                editor_pane_id: editor_pane_id.raw(),
            });
        }

        let panes = handoff_entries
            .iter()
            .map(|(_, runtime)| runtime.clone())
            .collect();
        let manifest = crate::server::handoff::manifest_for(
            snapshot,
            panes,
            dock_editors,
            params.expected_protocol,
            params.expected_version,
            self.api_window_title.clone(),
        );
        let mut import_child = match crate::server::handoff::spawn_handoff_import(
            import_exe.as_deref(),
            &socket_path,
            &token,
        ) {
            Ok(child) => child,
            Err(err) => {
                self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                return Err(err);
            }
        };
        let child_pid = import_child.id();
        info!(pid = child_pid, socket = %socket_path.display(), "spawned handoff import server");

        let mut fds = Vec::new();
        let duplicate_result = (|| {
            for (terminal_id, _) in &handoff_entries {
                let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) else {
                    continue;
                };
                fds.push(runtime.duplicate_handoff_fd()?);
            }
            Ok::<(), io::Error>(())
        })();
        if let Err(err) = duplicate_result {
            for fd in fds {
                let _ = unsafe { libc::close(fd) };
            }
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
            return Err(err);
        }

        let mut stream = match crate::server::handoff::accept_and_validate_on(
            listener,
            &socket_path,
            &token,
            &manifest,
        ) {
            Ok(stream) => stream,
            Err(err) => {
                for fd in fds {
                    let _ = unsafe { libc::close(fd) };
                }
                crate::server::handoff::cleanup_failed_import_child(&mut import_child);
                self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                return Err(err);
            }
        };

        let send_result = crate::server::handoff::send_fds_and_wait_restored(&mut stream, &fds);
        for fd in fds {
            let _ = unsafe { libc::close(fd) };
        }
        if let Err(err) = send_result {
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
            return Err(err);
        }

        if let Some(api_server) = &self.api_server {
            let _ = api_server.remove_socket_file_if_owned();
        } else {
            let _ = std::fs::remove_file(crate::api::socket_path());
        }
        let _ = remove_socket_file_if_owned(&self.client_socket_path, &self.client_socket_identity);
        if let Err(err) = crate::server::handoff::wait_ready(&mut stream) {
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            match self.wait_then_restore_public_sockets_after_failed_handoff() {
                Ok(()) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                }
                Err(restore_err) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                    return Err(io::Error::other(format!(
                        "handoff replacement server did not become ready: {err}; old server could not restore public sockets: {restore_err}"
                    )));
                }
            }
            return Err(io::Error::other(format!(
                "handoff replacement server did not become ready: {err}"
            )));
        }
        if let Err(err) = crate::server::handoff::report_committed(&mut stream) {
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            match self.wait_then_restore_public_sockets_after_failed_handoff() {
                Ok(()) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                }
                Err(restore_err) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                    return Err(io::Error::other(format!(
                        "handoff replacement server was ready, but commit failed: {err}; old server could not restore public sockets: {restore_err}"
                    )));
                }
            }
            return Err(err);
        }

        // Proxy panes have no local PTY to transfer. Once the replacement has
        // committed, release every remote lease and remove each ephemeral pane
        // before the old runtime registry is drained.
        self.teardown_remote_focus_proxies();

        let transferred: std::collections::HashSet<_> = handoff_entries
            .iter()
            .map(|(terminal_id, _)| terminal_id.clone())
            .collect();
        for (terminal_id, runtime) in self.app.terminal_runtimes.drain_for_handoff() {
            if !transferred.contains(&terminal_id) {
                continue;
            }
            debug!(terminal = %terminal_id, "preserving pane runtime for handoff");
            runtime.preserve_for_handoff();
        }
        crate::server::handoff::wait_owned_ack(&mut stream);

        Ok(())
    }

    fn finish_live_handoff_shutdown(&mut self) {
        self.teardown_remote_focus_proxies();
        self.shutting_down = true;
        self.app.state.should_quit = true;
        self.app.no_session = true;
        info!("live handoff completed; old server exiting");
    }

    /// Ends every proxy operation owned by this server. Proxy panes are
    /// ephemeral and cannot cross a handoff, so their pane, terminal state,
    /// runtime, and remote lease must leave together. The App runtime shutdown
    /// path owns the exactly-once lease release invariant.
    fn teardown_remote_focus_proxies(&mut self) {
        let proxy_terminal_ids = self
            .app
            .terminal_runtimes
            .iter()
            .filter(|(_, runtime)| runtime.is_remote_proxy())
            .map(|(terminal_id, _)| terminal_id.clone())
            .collect::<Vec<_>>();
        if proxy_terminal_ids.is_empty() {
            return;
        }

        let proxy_panes = self
            .app
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.panes.iter())
                    .filter_map(|(pane_id, pane)| {
                        proxy_terminal_ids
                            .contains(&pane.attached_terminal_id)
                            .then_some((*pane_id, pane.attached_terminal_id.clone()))
                    })
            })
            .collect::<Vec<_>>();

        for (pane_id, terminal_id) in proxy_panes {
            let Some(ws_idx) = self
                .app
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.pane_state(pane_id).is_some())
            else {
                continue;
            };
            let should_close_workspace = self
                .app
                .state
                .workspaces
                .get_mut(ws_idx)
                .is_some_and(|workspace| workspace.remove_pane(pane_id));
            if should_close_workspace {
                self.app.state.close_workspace_exact(ws_idx);
            }
            self.app.state.remove_unattached_terminal_ids([terminal_id]);
        }
        self.app
            .state
            .remove_unattached_terminal_ids(proxy_terminal_ids);
        self.app.state.mark_session_dirty();
        self.app.shutdown_detached_terminal_runtimes();
    }

    #[cfg(not(unix))]
    fn perform_live_handoff(
        &mut self,
        _params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        Err(io::Error::other("live handoff is only supported on Unix"))
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

    #[cfg(unix)]
    fn restore_public_sockets_after_failed_handoff(&mut self) -> io::Result<()> {
        let api_tx = self
            .api_tx
            .clone()
            .ok_or_else(|| io::Error::other("cannot restore api socket without api sender"))?;
        let api_server = api::start_server_with_stop_control(
            api_tx,
            self.app.event_hub.clone(),
            self.should_quit.clone(),
        )?;

        let client_path = client_socket_path();
        prepare_socket_path(&client_path)?;
        let listener = bind_local_listener(&client_path)?;
        restrict_socket_permissions(&client_path)?;
        let client_socket_identity = socket_file_identity(&client_path)?;
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

        self.api_server = Some(api_server);
        self.client_listener = listener;
        self.client_socket_path = client_path;
        self.client_socket_identity = client_socket_identity;
        Ok(())
    }

    #[cfg(unix)]
    fn wait_then_restore_public_sockets_after_failed_handoff(&mut self) -> io::Result<()> {
        let timeout = crate::server::handoff::COMMIT_TIMEOUT + Duration::from_secs(2);
        wait_for_old_public_sockets_to_close(timeout)?;
        self.restore_public_sockets_after_failed_handoff()
    }

    #[cfg(unix)]
    fn rollback_handoff_before_commit(
        &mut self,
        socket_path: &Path,
        paused_terminal_ids: &[crate::terminal::TerminalId],
    ) {
        for terminal_id in paused_terminal_ids {
            if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                runtime.set_handoff_reader_paused(false);
            }
        }
        self.handoff_in_progress = false;
        let _ = std::fs::remove_file(socket_path);
    }

    #[cfg(unix)]
    fn nudge_handoff_panes_on_first_client_attach(&mut self) {
        if !self.pending_handoff_repaint_nudge {
            return;
        }
        self.pending_handoff_repaint_nudge = false;
        self.app
            .terminal_runtimes
            .nudge_child_redraw_after_handoff();
    }

    #[cfg(not(unix))]
    fn nudge_handoff_panes_on_first_client_attach(&mut self) {}

    /// Give a fresh attach the dock state the server holds, so a client starts
    /// from the config rather than from the struct default.
    fn seed_client_dock_presentation(&mut self, client_id: u64) {
        let ignore_whitespace = self.app.state.dock_diff_ignore_whitespace;
        let default_surfaces = self.app.state.dock_default_surfaces.clone();
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.dock_presentation.diff_ignore_whitespace = ignore_whitespace;
            client.dock_presentation.tab = default_surfaces.first().copied();
            client.dock_presentation.open_surfaces = default_surfaces;
        }
    }

    /// A reloaded `ui.hide_whitespace_in_diff` reaches every attach, not only
    /// the one that happens to be swapped into `AppState`, and drops the diff
    /// each of them has rendered.
    fn apply_dock_diff_whitespace_to_clients(&mut self, ignore_whitespace: bool) {
        for client in self.clients.values_mut() {
            client.dock_presentation.diff_ignore_whitespace = ignore_whitespace;
            client.dock_presentation.diff_active_key = None;
            client.dock_presentation.diff_request = None;
        }
    }

    fn reload_server_config(&mut self, notify_success: bool) -> crate::config::ConfigReloadReport {
        let server_keybindings = self.server_keybindings.clone();
        apply_keybindings(&mut self.app, &server_keybindings);
        let previous_diff_whitespace = self.app.state.dock_diff_ignore_whitespace;
        let report = self.app.apply_config_from_disk(notify_success);
        if self.app.state.dock_diff_ignore_whitespace != previous_diff_whitespace {
            self.apply_dock_diff_whitespace_to_clients(self.app.state.dock_diff_ignore_whitespace);
        }
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
        let Some(policy) = self.client_presentation_policy(client_id) else {
            return false;
        };
        self.promote_client_to_foreground_with_policy(client_id, &policy)
    }

    fn sync_client_input_source(
        &mut self,
        client_id: u64,
        policy: &crate::app::state::ClientPresentationPolicy<'_>,
    ) {
        let active =
            self.app.state.switch_ascii_input_source_in_prefix && policy.mode().wants_ascii_input();
        self.send_to_client(client_id, ServerMessage::PrefixInputSource { active });
    }

    fn promote_client_to_foreground_with_policy(
        &mut self,
        client_id: u64,
        policy: &crate::app::state::ClientPresentationPolicy<'_>,
    ) -> bool {
        let stamp = self.allocate_activity_stamp();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        client.last_activity = stamp;

        let changed = self.foreground_client_id != Some(client_id);
        self.foreground_client_id = Some(client_id);
        self.sync_foreground_client_state();
        if changed {
            self.sync_client_input_source(client_id, policy);
        }
        changed
    }

    fn promote_latest_remaining_client(&mut self) -> bool {
        let next_foreground = latest_app_client(&self.clients);
        let changed = next_foreground != self.foreground_client_id;
        let next_policy =
            next_foreground.and_then(|client_id| self.client_presentation_policy(client_id));
        self.foreground_client_id = next_foreground;
        self.sync_foreground_client_state();
        if let (true, Some(client_id), Some(policy)) = (changed, next_foreground, next_policy) {
            self.sync_client_input_source(client_id, &policy);
        }
        changed
    }

    fn app_client_count(&self) -> usize {
        self.clients
            .values()
            .filter(|client| client.is_full_app_client() && client.writer.is_some())
            .count()
    }

    fn direct_graphics_available(&self) -> bool {
        self.app_client_count() == 1
            && self.foreground_client_id.is_some_and(|id| {
                self.clients.get(&id).is_some_and(|client| {
                    client.is_full_app_client() && client.writer.is_some() && client.direct_graphics
                })
            })
    }

    fn has_app_client(&self) -> bool {
        self.app_client_count() > 0
    }

    fn work_index_refresh_is_useful(&mut self, now: Instant) -> bool {
        if self.has_app_client() {
            self.last_app_client_seen = now;
            return true;
        }
        // Idleness only says nobody is watching the index go round on its own. A
        // refresh someone asked for has a reader by definition: a CLI-only user
        // who linked a merged pull request and is about to run `day list`.
        if self.app.work_index_refresh_requested {
            return true;
        }
        let idle_limit = Duration::from_secs(
            self.app
                .work_index_config
                .refresh_interval_seconds
                .max(1)
                .saturating_mul(6),
        );
        now.checked_duration_since(self.last_app_client_seen)
            .is_none_or(|idle| idle <= idle_limit)
    }

    fn has_renderable_status_target(&self) -> bool {
        self.clients.values().any(|client| {
            client.writer.is_some()
                && client.is_full_app_client()
                && crate::ui::status_bar_is_renderable(
                    &self.app.state,
                    Rect::new(0, 0, client.terminal_size.0, client.terminal_size.1),
                )
        })
    }

    fn remove_client(&mut self, client_id: u64) -> bool {
        let controlled_terminal = self.clients.get(&client_id).and_then(|client| {
            let ClientConnectionMode::TerminalAttach {
                terminal_id: _,
                control: Some(control),
            } = &client.mode
            else {
                return None;
            };
            Some((control.context.terminal_id.clone(), control.clone()))
        });
        if let Some((real_terminal_id, _lease)) = controlled_terminal {
            #[cfg(unix)]
            if let Some(real_terminal_id) = self.terminal_id_by_string(&real_terminal_id) {
                if let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) {
                    runtime.release_remote_owner(client_id);
                }
            }
            #[cfg(not(unix))]
            let _ = real_terminal_id;
        }
        self.retire_direct_graphics_for_client(client_id);
        let was_foreground = self.foreground_client_id == Some(client_id);
        self.app.clear_input_source(client_id);
        self.send_client_graphics_cleanup(client_id);
        let removed = self.clients.remove(&client_id);
        if let Some(removed) = removed {
            crate::server::clipboard_image::remove_files(removed.staged_clipboard_files);
            if let ClientConnectionMode::TerminalAttach { terminal_id, .. } = removed.mode {
                self.terminal_attach_owners.remove(&terminal_id);
                if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
                    self.app
                        .state
                        .direct_attach_resize_locks
                        .remove(&terminal_id);
                }
            }
        }
        if was_foreground {
            self.promote_latest_remaining_client()
        } else {
            false
        }
    }

    fn client_removal_needs_shared_resize(&self, client_id: u64) -> bool {
        if self.foreground_client_id == Some(client_id) {
            return true;
        }
        matches!(
            self.clients.get(&client_id).map(|client| &client.mode),
            Some(
                ClientConnectionMode::TerminalAttach { .. }
                    | ClientConnectionMode::TerminalObserve { .. }
            )
        ) && self.foreground_client_id.is_some()
    }

    fn remove_client_and_resize_if_needed(&mut self, client_id: u64) {
        let affected = self.client_terminal_ids(client_id);
        let needs_shared_resize = self.client_removal_needs_shared_resize(client_id);
        let foreground_changed = self.remove_client(client_id);
        if needs_shared_resize || foreground_changed {
            self.resize_shared_runtime_to_effective_size();
        }
        self.reconcile_terminal_geometries(affected);
    }

    fn send_client_graphics_cleanup(&mut self, client_id: u64) {
        let (writer, bytes) = match self.clients.get_mut(&client_id) {
            Some(client) => {
                let bytes = client.graphics_cache.clear_bytes();
                (client.writer.as_ref().cloned(), bytes)
            }
            None => return,
        };
        if bytes.is_empty() {
            return;
        }
        let Some(writer) = writer else {
            return;
        };
        let Ok(serialized) = Self::frame_server_message(&ServerMessage::Graphics { bytes }) else {
            return;
        };
        writer.replace_with_cleanup(serialized);
    }

    fn send_all_clients_graphics_cleanup(&mut self) {
        let client_ids = self.clients.keys().copied().collect::<Vec<_>>();
        for client_id in client_ids {
            self.send_client_graphics_cleanup(client_id);
        }
    }

    fn update_client_host_theme_from_events(
        &mut self,
        client_id: u64,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };

        if !client.update_host_theme_from_events(events) {
            return false;
        }

        if self.foreground_client_id == Some(client_id) {
            let mut changed = self.app.set_host_terminal_appearance_state(
                client.host_terminal_appearance,
                client.host_terminal_appearance_explicit,
            );
            changed |= self.app.set_host_terminal_theme(client.host_terminal_theme);
            if changed {
                self.resize_shared_runtime_to_effective_size_before_input();
            }
            changed
        } else {
            false
        }
    }

    fn update_client_outer_focus_from_events(
        &mut self,
        client_id: u64,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let Some(next_focus) = client.update_outer_focus_from_events(events) else {
            return false;
        };
        if self.foreground_client_id == Some(client_id) {
            self.app.state.outer_terminal_focus = Some(next_focus);
        }
        self.app_clients_host_focused() && self.app.state.pomodoro.resume_held(Instant::now())
    }

    fn app_clients_host_focused(&self) -> bool {
        crate::server::clients::aggregate_outer_focus(
            self.clients
                .values()
                .filter(|client| client.is_full_app_client() && client.writer.is_some())
                .map(|client| client.outer_terminal_focus),
        )
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
            if let ServerEvent::ClientConnected { writer, .. } = event {
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
            .find(|id| id.as_str() == terminal_id)
            .cloned()
    }

    fn runtime_for_terminal_id_string(
        &self,
        terminal_id: &str,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        let terminal_id = self
            .app
            .state
            .terminals
            .keys()
            .find(|id| id.as_str() == terminal_id)?;
        self.app.terminal_runtimes.get(terminal_id)
    }

    fn forward_terminal_attach_bytes(
        &mut self,
        terminal_id: &str,
        data: Vec<u8>,
        reset_scroll: bool,
    ) -> Option<Result<(), String>> {
        self.app.begin_contract_false_positive_input_burst();
        let terminal_id = self.terminal_id_by_string(terminal_id)?;
        let data = Bytes::from(data);
        let has_bytes = !data.is_empty();
        let result = {
            let runtime = self.app.terminal_runtimes.get(&terminal_id)?;
            if reset_scroll {
                runtime.scroll_reset();
            }
            runtime
                .try_send_bytes(data.clone())
                .map_err(|err| err.to_string())
        };
        if result.is_ok() && has_bytes {
            if let Some(pane_id) = self.app.state.pane_id_for_terminal(&terminal_id) {
                self.app.note_human_bytes(pane_id, &data);
            }
            self.app.retire_blocked_hook_authority_for_terminal(
                &terminal_id,
                std::time::Instant::now(),
            );
        }
        Some(result)
    }

    /// Revalidate the authoritative remote context and write one input batch
    /// directly to the PTY master during this server event-loop turn.
    #[cfg(unix)]
    fn controlled_write_target(
        &self,
        client_id: u64,
    ) -> Result<
        Option<(
            crate::server::remote_control::RemoteControlLease,
            crate::terminal::TerminalId,
        )>,
        crate::api::schema::ErrorBody,
    > {
        let Some(lease) = self
            .clients
            .get(&client_id)
            .and_then(|client| match &client.mode {
                ClientConnectionMode::TerminalAttach {
                    control: Some(control),
                    ..
                } => Some((**control).clone()),
                _ => None,
            })
        else {
            return Ok(None);
        };
        crate::server::remote_control::validate_input_owner(
            self.terminal_attach_owners
                .get(&lease.context.terminal_id)
                .copied(),
            client_id,
        )?;
        let real_terminal_id = self
            .terminal_id_by_string(&lease.context.terminal_id)
            .ok_or_else(|| crate::api::schema::ErrorBody {
                code: "connection_lost".to_owned(),
                message: "controlled terminal no longer exists; delivery is unknown".to_owned(),
            })?;
        if self.app.terminal_runtimes.get(&real_terminal_id).is_none() {
            return Err(crate::api::schema::ErrorBody {
                code: "connection_lost".to_owned(),
                message: "controlled terminal runtime is gone; delivery is unknown".to_owned(),
            });
        }
        Ok(Some((lease, real_terminal_id)))
    }

    #[cfg(unix)]
    fn forward_control_bytes(&mut self, client_id: u64, data: Vec<u8>) -> bool {
        let target = match self.controlled_write_target(client_id) {
            Ok(Some(target)) => target,
            Ok(None) => return false,
            Err(error) => {
                self.reject_remote_control(client_id, error);
                return false;
            }
        };
        let (lease, real_terminal_id) = target;
        let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) else {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "connection_lost".to_owned(),
                    message: "controlled terminal runtime is gone; delivery is unknown".to_owned(),
                },
            );
        };
        runtime.scroll_reset();
        let provider: &dyn crate::server::remote_control::RemoteControlContextProvider = &self.app;
        let current = match provider.fresh_remote_control_context(&lease.agent_ref) {
            Ok(context) => context,
            Err(error) => {
                self.reject_remote_control(client_id, error);
                return false;
            }
        };
        self.forward_control_bytes_with_current_context(
            client_id,
            data,
            lease,
            real_terminal_id,
            current,
        )
    }

    #[cfg(unix)]
    fn forward_control_bytes_with_current_context(
        &mut self,
        client_id: u64,
        data: Vec<u8>,
        lease: crate::server::remote_control::RemoteControlLease,
        real_terminal_id: crate::terminal::TerminalId,
        current: api::schema::RemoteControlContext,
    ) -> bool {
        if let Err(error) = crate::server::remote_control::validate_context(
            &self.app.state.agent_host_name,
            &lease.context.user,
            &lease.context,
            &current,
        ) {
            self.reject_remote_control(client_id, error);
            return false;
        }
        let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) else {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "connection_lost".to_owned(),
                    message: "controlled terminal runtime is gone; delivery is unknown".to_owned(),
                },
            );
        };
        let has_bytes = !data.is_empty();
        let result = runtime.try_send_controlled_bytes(client_id, &data);
        self.finish_controlled_write(client_id, &real_terminal_id, has_bytes, result)
    }

    #[cfg(unix)]
    fn finish_controlled_write(
        &mut self,
        client_id: u64,
        real_terminal_id: &crate::terminal::TerminalId,
        has_bytes: bool,
        result: crate::pty::actor::ControlledWriteResult,
    ) -> bool {
        match result {
            crate::pty::actor::ControlledWriteResult::Written => {
                if has_bytes {
                    self.app.retire_blocked_hook_authority_for_terminal(
                        real_terminal_id,
                        std::time::Instant::now(),
                    );
                }
                true
            }
            crate::pty::actor::ControlledWriteResult::Refused => {
                self.reject_remote_control(
                    client_id,
                    crate::api::schema::ErrorBody {
                        code: "refused_for_safety".to_owned(),
                        message: "controlled PTY write gate refused the batch".to_owned(),
                    },
                );
                false
            }
            crate::pty::actor::ControlledWriteResult::DeliveryUnknown { written } => {
                self.reject_remote_control(
                    client_id,
                    crate::api::schema::ErrorBody {
                        code: "connection_lost".to_owned(),
                        message: format!(
                            "controlled PTY delivery became unknown after {written} bytes; no retry",
                        ),
                    },
                );
                false
            }
        }
    }

    #[cfg(all(test, unix))]
    fn forward_control_bytes_with_provider_for_test(
        &mut self,
        client_id: u64,
        data: Vec<u8>,
        provider: &dyn crate::server::remote_control::RemoteControlContextProvider,
    ) -> bool {
        let target = match self.controlled_write_target(client_id) {
            Ok(Some(target)) => target,
            Ok(None) => return false,
            Err(error) => {
                self.reject_remote_control(client_id, error);
                return false;
            }
        };
        let (lease, real_terminal_id) = target;
        let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) else {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "connection_lost".to_owned(),
                    message: "controlled terminal runtime is gone; delivery is unknown".to_owned(),
                },
            );
        };
        runtime.scroll_reset();
        let current = match provider.fresh_remote_control_context(&lease.agent_ref) {
            Ok(context) => context,
            Err(error) => {
                self.reject_remote_control(client_id, error);
                return false;
            }
        };
        self.forward_control_bytes_with_current_context(
            client_id,
            data,
            lease,
            real_terminal_id,
            current,
        )
    }

    #[cfg(not(unix))]
    fn forward_control_bytes(&mut self, client_id: u64, _data: Vec<u8>) -> bool {
        self.reject_remote_control(
            client_id,
            crate::api::schema::ErrorBody {
                code: "agent_not_attachable".to_owned(),
                message: "remote control is supported only on Unix runtimes".to_owned(),
            },
        )
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

    fn write_client_clipboard_image(
        &mut self,
        client_id: u64,
        extension: &str,
        data: &[u8],
    ) -> std::io::Result<String> {
        let staged = crate::server::clipboard_image::stage(client_id, extension, data)?;
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.staged_clipboard_files.push(staged.path);
        }
        info!(client_id, bytes = data.len(), path = %staged.paste_text, "staged client clipboard image");
        Ok(staged.paste_text)
    }

    fn paste_client_clipboard_image_path(&mut self, client_id: u64, path: String) -> bool {
        let attached_terminal_id = self.clients.get(&client_id).and_then(|client| {
            if let ClientConnectionMode::TerminalAttach { terminal_id, .. } = &client.mode {
                Some(terminal_id.clone())
            } else {
                None
            }
        });
        if let Some(terminal_id) = attached_terminal_id {
            if let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) {
                let payload = paste_payload_for_runtime(runtime, &path);
                let guarded = self.clients.get(&client_id).is_some_and(|client| {
                    matches!(
                        client.mode,
                        ClientConnectionMode::TerminalAttach {
                            control: Some(_),
                            ..
                        }
                    )
                });
                if guarded {
                    return self.forward_control_bytes(client_id, payload.into_bytes());
                }
                if let Some(Err(err)) =
                    self.forward_terminal_attach_bytes(&terminal_id, payload.into_bytes(), false)
                {
                    warn!(client_id, terminal_id = %terminal_id, err = %err, "terminal attach clipboard image paste failed");
                }
            }
            return true;
        }

        self.handle_client_input_events(
            client_id,
            vec![crate::raw_input::RawInputEvent::Paste(path)],
        );
        true
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
        let mut affected = client
            .terminal_geometries
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        client.terminal_geometries.clear();
        client.mode = ClientConnectionMode::TerminalObserve {
            terminal_id: terminal_id.clone(),
        };
        client.pending_terminal_attach = false;
        client.render_state.reset_baseline();
        client.last_activity = stamp;
        let was_foreground = self.foreground_client_id == Some(client_id);
        if was_foreground {
            self.promote_latest_remaining_client();
        }

        info!(client_id, cols, rows, terminal_id = %terminal_id, "terminal observe client connected");
        if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
            affected.insert(terminal_id);
        }
        self.reconcile_terminal_geometries(affected);
        true
    }

    fn reject_remote_control(
        &mut self,
        client_id: u64,
        error: crate::api::schema::ErrorBody,
    ) -> bool {
        self.send_to_client(
            client_id,
            ServerMessage::ControlError {
                code: error.code,
                message: error.message,
            },
        );
        self.remove_client_and_resize_if_needed(client_id);
        false
    }

    fn control_terminal_client(
        &mut self,
        client_id: u64,
        target: String,
        agent_ref: Option<crate::api::schema::AgentRef>,
        expected_context: Option<Box<crate::api::schema::RemoteControlContext>>,
        takeover: bool,
    ) -> bool {
        let Some(agent_ref) = agent_ref else {
            return self.control_terminal_client_legacy(client_id, target, takeover);
        };

        #[cfg(not(unix))]
        {
            let _ = (agent_ref, expected_context);
            self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "agent_not_attachable".to_owned(),
                    message: "remote control is supported only on Unix runtimes".to_owned(),
                },
            )
        }
        #[cfg(unix)]
        {
            self.control_terminal_client_guarded(client_id, agent_ref, expected_context, takeover)
        }
    }

    #[cfg(unix)]
    fn control_terminal_client_guarded(
        &mut self,
        client_id: u64,
        agent_ref: crate::api::schema::AgentRef,
        expected_context: Option<Box<crate::api::schema::RemoteControlContext>>,
        takeover: bool,
    ) -> bool {
        if takeover {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "already_controlled".to_owned(),
                    message: "remote control takeover is not supported".to_owned(),
                },
            );
        }
        if !self.client_is_pending_terminal_mode(client_id) {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "agent_not_attachable".to_owned(),
                    message: "connection is not pending terminal control".to_owned(),
                },
            );
        }

        let provider: &dyn crate::server::remote_control::RemoteControlContextProvider = &self.app;
        let current = match provider.fresh_remote_control_context(&agent_ref) {
            Ok(context) => context,
            Err(error) => return self.reject_remote_control(client_id, error),
        };
        let expected = expected_context
            .map(|context| *context)
            .unwrap_or_else(|| current.clone());
        let Some(effective_user) = crate::platform::effective_user_name() else {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "refused_for_safety".to_owned(),
                    message: "effective remote user is unavailable".to_owned(),
                },
            );
        };
        if let Err(error) = crate::server::remote_control::validate_context(
            &self.app.state.agent_host_name,
            &effective_user,
            &expected,
            &current,
        ) {
            return self.reject_remote_control(client_id, error);
        }
        let Some(real_terminal_id) = self.terminal_id_by_string(&current.terminal_id) else {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "agent_not_attachable".to_owned(),
                    message: "controlled terminal no longer exists".to_owned(),
                },
            );
        };
        if self
            .pending_alt_screen_reads
            .iter()
            .any(|pending| pending.terminal_id == real_terminal_id)
        {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "refused_for_safety".to_owned(),
                    message: "terminal has a read in progress".to_owned(),
                },
            );
        }
        if self
            .terminal_attach_owners
            .get(&current.terminal_id)
            .is_some_and(|owner| *owner != client_id)
        {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "already_controlled".to_owned(),
                    message: "terminal already has a writable controller".to_owned(),
                },
            );
        }
        let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) else {
            return self.reject_remote_control(
                client_id,
                crate::api::schema::ErrorBody {
                    code: "agent_not_attachable".to_owned(),
                    message: "controlled terminal runtime is gone".to_owned(),
                },
            );
        };
        match runtime.try_acquire_remote_owner(client_id) {
            crate::pty::actor::RemoteOwnerAcquireResult::Acquired => {}
            crate::pty::actor::RemoteOwnerAcquireResult::AlreadyControlled => {
                return self.reject_remote_control(
                    client_id,
                    crate::api::schema::ErrorBody {
                        code: "already_controlled".to_owned(),
                        message: "controlled terminal runtime already has a writer".to_owned(),
                    },
                );
            }
            crate::pty::actor::RemoteOwnerAcquireResult::RefusedForSafety => {
                return self.reject_remote_control(
                    client_id,
                    crate::api::schema::ErrorBody {
                        code: "refused_for_safety".to_owned(),
                        message: "local terminal input is still in flight".to_owned(),
                    },
                );
            }
        }
        let lease = crate::server::remote_control::RemoteControlLease {
            agent_ref,
            context: current.clone(),
        };
        let lease_for_attach = lease.clone();
        if !self.attach_terminal_client_with_control(
            client_id,
            current.terminal_id.clone(),
            false,
            Some(Box::new(lease_for_attach)),
        ) {
            if let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) {
                runtime.release_remote_owner(client_id);
            }
            return false;
        }
        self.send_to_client(
            client_id,
            ServerMessage::ControlReady {
                context: Box::new(current),
            },
        );
        true
    }

    fn control_terminal_client_legacy(
        &mut self,
        client_id: u64,
        target: String,
        takeover: bool,
    ) -> bool {
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
        self.app.begin_contract_false_positive_input_burst();
        let Some(ClientConnection {
            mode:
                ClientConnectionMode::TerminalAttach {
                    terminal_id,
                    control: None,
                },
            ..
        }) = self.clients.get(&client_id)
        else {
            return false;
        };
        let terminal_id = terminal_id.clone();
        let Some(resolved_terminal_id) = self.terminal_id_by_string(&terminal_id) else {
            return false;
        };
        let Some(runtime) = self.app.terminal_runtimes.get(&resolved_terminal_id) else {
            return false;
        };

        if let Err(err) =
            apply_terminal_attach_scroll(runtime, source, direction, lines, column, row, modifiers)
        {
            warn!(client_id, terminal_id = %terminal_id, err = %err, "terminal attach scroll failed");
        }
        true
    }

    fn pane_effective_state(&self, pane_id: crate::layout::PaneId) -> crate::detect::AgentState {
        self.app
            .state
            .workspaces
            .iter()
            .find_map(|ws| {
                ws.tabs.iter().find_map(|tab| {
                    let pane = tab.panes.get(&pane_id)?;
                    self.app
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .map(|terminal| terminal.raw_agent_state())
                })
            })
            .unwrap_or(crate::detect::AgentState::Unknown)
    }

    fn pane_effective_agent_label(&self, pane_id: crate::layout::PaneId) -> Option<String> {
        self.app.state.workspaces.iter().find_map(|ws| {
            ws.tabs.iter().find_map(|tab| {
                let pane = tab.panes.get(&pane_id)?;
                self.app
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .and_then(|terminal| terminal.effective_agent_label())
                    .map(str::to_string)
            })
        })
    }

    fn forward_pane_state_update_notifications_to_clients(
        &mut self,
        update: &crate::app::actions::PaneStateUpdate,
    ) {
        if self.app.state.toast_config.delay_seconds != 0 {
            return;
        }

        let is_active_tab = self
            .app
            .state
            .pane_is_in_active_tab(update.ws_idx, update.pane_id);
        let suppress_active_tab_notifications =
            self.active_tab_suppresses_notifications(is_active_tab);

        if !update.suppress_completion && self.app.state.sound.allows(update.known_agent) {
            if let Some(sound) =
                crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                    suppress_active_tab_notifications,
                    update.previous_state,
                    update.state,
                    update.previous_agent_label.as_deref(),
                    update.agent_label.as_deref(),
                )
            {
                self.send_notify_to_foreground_client(
                    protocol::NotifyKind::Sound,
                    sound_notify_message(sound),
                    None,
                );
            }
        }

        if !should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
            return;
        }
        let Some(kind) = crate::app::actions::notification_toast_for_pane_state_update(
            suppress_active_tab_notifications,
            update,
        ) else {
            return;
        };
        let Some(ws) = self.app.state.workspaces.get(update.ws_idx) else {
            return;
        };
        let Some(agent_label) = update.agent_label.as_deref() else {
            return;
        };
        let event_text = match kind {
            crate::app::state::ToastKind::NeedsAttention => "needs attention",
            crate::app::state::ToastKind::Finished => "finished",
            crate::app::state::ToastKind::UpdateInstalled => "updated",
            crate::app::state::ToastKind::WorkLinked => "linked",
        };
        let workspace_label =
            ws.display_name_from(&self.app.state.terminals, &self.app.terminal_runtimes);
        let context = crate::app::actions::notification_context(
            ws,
            &self.app.state.terminals,
            &workspace_label,
            update.ws_idx,
            update.pane_id,
        );
        self.send_notify_to_foreground_client(
            toast_notify_kind(self.app.state.toast_config.delivery)
                .expect("toast forwarding requires a client notification kind"),
            format!("{agent_label} {event_text}"),
            non_empty_body(&context),
        );
    }

    fn forward_agent_notification_delivery(
        &mut self,
        delivery: &crate::app::state::AgentNotificationDelivery,
    ) {
        if let Some(sound) = delivery.sound {
            self.send_notify_to_foreground_client(
                protocol::NotifyKind::Sound,
                sound_notify_message(sound),
                None,
            );
        }

        if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
            if let Some(toast) = &delivery.client_notification {
                self.send_notify_to_foreground_client(
                    toast_notify_kind(self.app.state.toast_config.delivery)
                        .expect("toast forwarding requires a client notification kind"),
                    &toast.title,
                    non_empty_body(&toast.context),
                );
            }
        }
    }

    fn send_notify_to_foreground_client(
        &mut self,
        kind: protocol::NotifyKind,
        message: impl Into<String>,
        body: Option<String>,
    ) -> bool {
        self.send_to_foreground_client(ServerMessage::Notify {
            kind,
            message: message.into(),
            body,
        })
    }

    fn send_flat_toast_to_foreground_client(
        &mut self,
        kind: protocol::NotifyKind,
        message: impl AsRef<str>,
    ) -> bool {
        let (title, body) = crate::terminal_notify::split_message(message.as_ref());
        self.send_notify_to_foreground_client(kind, title, body.map(str::to_string))
    }

    fn handle_notification_show_api(
        &mut self,
        id: String,
        params: api::schema::NotificationShowParams,
    ) -> String {
        use api::schema::{NotificationShowReason, ResponseResult};

        let Some(title) = sanitize_notification_text(&params.title, 80) else {
            return serde_json::to_string(&api::schema::ErrorResponse {
                id,
                error: api::schema::ErrorBody {
                    code: "invalid_params".into(),
                    message: "notification title is empty".into(),
                },
            })
            .unwrap_or_else(|_| "{}".to_string());
        };

        match self.app.state.toast_config.delivery {
            config::ToastDelivery::Off => {
                return serde_json::to_string(&api::schema::SuccessResponse {
                    id,
                    result: ResponseResult::NotificationShow {
                        shown: false,
                        reason: NotificationShowReason::Disabled,
                    },
                })
                .unwrap_or_else(|_| "{}".to_string());
            }
            config::ToastDelivery::Herdr => {
                let sound = params.sound;
                let response = self.app.handle_api_request_after_internal_events_drained(
                    api::schema::Request {
                        id,
                        method: api::schema::Method::NotificationShow(params),
                    },
                );
                if notification_show_response_shown(&response) {
                    self.forward_api_notification_sound(sound);
                }
                return response;
            }
            config::ToastDelivery::Terminal | config::ToastDelivery::System => {}
        }

        let body = params
            .body
            .as_deref()
            .and_then(|body| sanitize_notification_text(body, 240));
        if self.app.api_notification_rate_limited(Instant::now()) {
            return serde_json::to_string(&api::schema::SuccessResponse {
                id,
                result: ResponseResult::NotificationShow {
                    shown: false,
                    reason: NotificationShowReason::RateLimited,
                },
            })
            .unwrap_or_else(|_| "{}".to_string());
        }
        let kind = toast_notify_kind(self.app.state.toast_config.delivery)
            .expect("terminal/system delivery has notify kind");
        let shown = self.send_notify_to_foreground_client(kind, title, body);
        if shown {
            self.app.mark_api_notification_shown(Instant::now());
            self.forward_api_notification_sound(params.sound);
        }
        let reason = if shown {
            NotificationShowReason::Shown
        } else {
            NotificationShowReason::NoForegroundClient
        };

        serde_json::to_string(&api::schema::SuccessResponse {
            id,
            result: ResponseResult::NotificationShow { shown, reason },
        })
        .unwrap_or_else(|_| "{}".to_string())
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
        let changes = self.app.sync_terminal_titles();
        let outer_title_synced = focused_source && self.app.window_title_uses_terminal_title();
        if outer_title_synced {
            self.sync_window_title();
        }
        (
            changes.chrome_changed
                || (changes.raw_changed && self.app.terminal_title_sidebar_configured()),
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

    fn forward_api_notification_sound(&mut self, sound: api::schema::NotificationShowSound) {
        let Some(sound) = sound.to_sound() else {
            return;
        };
        self.send_notify_to_foreground_client(
            protocol::NotifyKind::Sound,
            sound_notify_message(sound),
            None,
        );
    }

    /// Handles a single internal event with forwarding logic for clipboard,
    /// sound, and toast notifications to connected clients.
    ///
    /// ALL internal events MUST be routed through this method to ensure
    /// clipboard/notify forwarding is never bypassed. Do not call
    /// `self.app.handle_internal_event()` directly for any internal event
    /// in the headless server — use this method instead.
    ///
    /// Returns true if the event changed visual state (requiring a re-render).
    fn handle_internal_event_with_forwarding(&mut self, ev: AppEvent) -> bool {
        let overlay_owner = match &ev {
            AppEvent::WorktreeAddFinished(result) => result
                .api_request
                .as_ref()
                .and_then(|request| request.client_id),
            AppEvent::WorktreeRemoveFinished(result) => result
                .api_request
                .as_ref()
                .and_then(|request| request.client_id),
            _ => None,
        };
        if let Some(client_id) = overlay_owner {
            return self.handle_client_owned_worktree_event(client_id, ev);
        }
        match &ev {
            AppEvent::FleetRefreshed { .. } | AppEvent::FleetAgentInventoryChanged { .. } => {
                let changed = self.app.handle_internal_event_with_render_impact(ev);
                let remote_entries = &self.app.state.remote_agent_panel_entries;
                let mut client_changed = false;
                for client in self.clients.values_mut() {
                    let selection_is_gone = client
                        .sidebar_presentation
                        .selected_remote_agent
                        .as_ref()
                        .is_some_and(|selected| {
                            !remote_entries
                                .iter()
                                .any(|entry| &entry.agent_ref == selected)
                        });
                    if selection_is_gone {
                        client.sidebar_presentation.selected_remote_agent = None;
                        client.request_repaint();
                        client_changed = true;
                    }
                }
                changed || client_changed
            }
            #[cfg(unix)]
            AppEvent::RemoteControlGatePoisoned { pane_id } => {
                let controlled_clients: Vec<u64> = self
                    .clients
                    .iter()
                    .filter_map(|(client_id, client)| {
                        let ClientConnectionMode::TerminalAttach {
                            control: Some(control),
                            ..
                        } = &client.mode
                        else {
                            return None;
                        };
                        (self
                            .terminal_id_by_string(&control.context.terminal_id)
                            .is_some_and(|terminal_id| {
                                self.app.state.workspaces.iter().any(|workspace| {
                                    workspace
                                        .terminal_id(*pane_id)
                                        .is_some_and(|candidate| candidate == &terminal_id)
                                })
                            }))
                        .then_some(*client_id)
                    })
                    .collect();
                for client_id in controlled_clients {
                    self.reject_remote_control(
                        client_id,
                        crate::api::schema::ErrorBody {
                            code: "refused_for_safety".to_owned(),
                            message: "PTY user-write gate is poisoned; remote control is disabled while the pane remains alive".to_owned(),
                        },
                    );
                }
                false
            }
            AppEvent::TerminalBell { pane_id, count } => {
                if !self.send_to_foreground_client(ServerMessage::TerminalBell { count: *count }) {
                    debug!(
                        pane = pane_id.raw(),
                        count, "dropped terminal bell without a foreground client"
                    );
                }
                false
            }
            AppEvent::ClipboardWrite { content } => {
                // Clipboard writes are client-local side effects. Forward them only to
                // the foreground client instead of broadcasting to every attached client.
                let data = base64::engine::general_purpose::STANDARD.encode(content.as_slice());
                if self.send_to_foreground_client(ServerMessage::Clipboard { data }) {
                    self.app.show_clipboard_feedback();
                }
                true
            }
            AppEvent::PrefixInputSource { client_id, active } => {
                // A queued transition stays with the client whose presentation produced it.
                // Monolithic-origin events have no client id and retain the foreground fallback.
                if let Some(client_id) = client_id {
                    self.send_to_client(
                        *client_id,
                        ServerMessage::PrefixInputSource { active: *active },
                    );
                } else {
                    self.send_to_foreground_client(ServerMessage::PrefixInputSource {
                        active: *active,
                    });
                }
                true
            }
            AppEvent::StateChanged { pane_id, agent, .. } => {
                // Capture toast before handling.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = *agent;

                // Find the previous effective state of this pane before the event
                // is processed. Notifications must follow effective state changes,
                // not raw fallback reports that may be masked by hook authority.
                let prev_state = self.pane_effective_state(pane_id_val);
                let prev_agent_label = self.pane_effective_agent_label(pane_id_val);

                // Handle the state change (updates pane state, sets toast on AppState).
                // Headless mode disables local sound playback separately from the
                // sound policy so reloads can keep server-side notification policy live.
                self.sync_foreground_client_state();
                let suppress_completion = self
                    .app
                    .handle_internal_event_with_pane_updates(ev)
                    .iter()
                    .any(|update| update.pane_id == pane_id_val && update.suppress_completion);

                // Forward sound notification to clients when server-side sound policy allows it.
                let is_active_tab = self
                    .app
                    .state
                    .active
                    .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| {
                        ws.find_tab_index_for_pane(pane_id_val)
                            .is_some_and(|tab_idx| ws.active_tab_index() == tab_idx)
                    });

                let suppress_active_tab_notifications =
                    self.active_tab_suppresses_notifications(is_active_tab);

                let next_state = self.pane_effective_state(pane_id_val);
                let next_agent_label = self.pane_effective_agent_label(pane_id_val);

                if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && self.app.state.sound.allows(agent_val)
                {
                    if let Some(sound) =
                        crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                            next_agent_label.as_deref(),
                        )
                    {
                        self.send_notify_to_foreground_client(
                            protocol::NotifyKind::Sound,
                            sound_notify_message(sound),
                            None,
                        );
                    }
                }

                let toast_msg = if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                {
                    if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                        self.app
                            .state
                            .toast
                            .as_ref()
                            .map(|toast| format!("{}: {}", toast.title, toast.context))
                    } else {
                        toast_message_from_state_change(
                            &self.app.state,
                            &self.app.terminal_runtimes,
                            pane_id_val,
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                        )
                    }
                } else {
                    None
                };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::HookStateReported {
                pane_id,
                agent_label,
                ..
            } => {
                // Hook reports can be stale or no-op after sequence rejection.
                // Forward only effective state changes observed after handling.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = crate::detect::parse_agent_label(agent_label);

                // Capture the previous effective state for this pane. Hook reports
                // are already folded into pane.state; raw hook transitions must not
                // produce a second notification path.
                let prev_state = self.pane_effective_state(pane_id_val);
                let prev_agent_label = self.pane_effective_agent_label(pane_id_val);

                self.sync_foreground_client_state();
                let suppress_completion = self
                    .app
                    .handle_internal_event_with_pane_updates(ev)
                    .iter()
                    .any(|update| update.pane_id == pane_id_val && update.suppress_completion);

                // Forward sound notification based on the effective transition when
                // server-side sound policy allows it.
                let is_active_tab = self
                    .app
                    .state
                    .active
                    .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| {
                        ws.find_tab_index_for_pane(pane_id_val)
                            .is_some_and(|tab_idx| ws.active_tab_index() == tab_idx)
                    });

                let suppress_active_tab_notifications =
                    self.active_tab_suppresses_notifications(is_active_tab);

                let next_state = self.pane_effective_state(pane_id_val);
                let next_agent_label = self.pane_effective_agent_label(pane_id_val);

                if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && self.app.state.sound.allows(agent_val)
                {
                    if let Some(sound) =
                        crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                            next_agent_label.as_deref(),
                        )
                    {
                        self.send_notify_to_foreground_client(
                            protocol::NotifyKind::Sound,
                            sound_notify_message(sound),
                            None,
                        );
                    }
                }

                let toast_msg = if !suppress_completion
                    && self.app.state.toast_config.delay_seconds == 0
                    && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                {
                    if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                        self.app
                            .state
                            .toast
                            .as_ref()
                            .map(|toast| format!("{}: {}", toast.title, toast.context))
                    } else {
                        toast_message_from_state_change(
                            &self.app.state,
                            &self.app.terminal_runtimes,
                            pane_id_val,
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                        )
                    }
                } else {
                    None
                };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::UpdateReady {
                version,
                install_command,
            } => {
                let toast_before = self.app.state.toast.clone();
                let version = version.clone();
                let install_command = install_command.clone();

                self.app.handle_internal_event(ev);

                let toast_msg =
                    if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
                        if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                            self.app
                                .state
                                .toast
                                .as_ref()
                                .map(|toast| format!("{}: {}", toast.title, toast.context))
                        } else {
                            Some(format!(
                                "v{version} available: {}",
                                crate::update::update_install_instruction(&install_command)
                            ))
                        }
                    } else {
                        None
                    };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::PaneDied { pane_id } => {
                let pane_id_val = *pane_id;
                if self.app.pane_runtime_is_suspended(pane_id_val) {
                    return true;
                }
                let terminal_id = self.app.state.workspaces.iter().find_map(|ws| {
                    ws.tabs.iter().find_map(|tab| {
                        tab.panes
                            .get(pane_id)
                            .map(|pane| pane.attached_terminal_id.to_string())
                    })
                });
                if let Some(update) = self
                    .app
                    .state
                    .publish_pane_process_exit_if_agent(pane_id_val)
                {
                    self.app.emit_pane_state_update(&update);
                    self.forward_pane_state_update_notifications_to_clients(&update);
                }

                self.app.handle_internal_event(ev);

                if self.app.find_pane(pane_id_val).is_none() {
                    if let Some(terminal_id) = terminal_id {
                        self.shutdown_terminal_stream_clients(
                            &terminal_id,
                            format!("terminal {terminal_id} exited"),
                        );
                    }
                }
                let _ = self.finish_pending_client_pane_focus();

                true
            }
            AppEvent::LoopRunHistoryChanged => {
                let changed = self.app.handle_internal_event_with_render_impact(ev);
                if changed {
                    self.refresh_client_loop_history_details();
                }
                changed
            }
            AppEvent::SymphonyWorkflowsRefreshed { .. } => {
                let changed = self.app.handle_internal_event_with_render_impact(ev);
                let dashboard_open = self
                    .clients
                    .values()
                    .any(|client| client.symphony_detail.is_some());
                self.refresh_client_symphony_details();
                changed || dashboard_open
            }
            AppEvent::WorkIndexRefreshed { .. } => {
                let changed = self.app.handle_internal_event_with_render_impact(ev);
                let view_open = self
                    .clients
                    .values()
                    .any(|client| client.work_view.is_some());
                self.refresh_client_work_views();
                changed || view_open
            }
            AppEvent::UsageScanFinished { .. } => {
                let changed = self.app.handle_internal_event_with_render_impact(ev);
                let view_open = self
                    .clients
                    .values()
                    .any(|client| client.usage_view.is_some());
                self.refresh_client_usage_views();
                changed || view_open
            }
            AppEvent::WorkItemDetailRefreshed { .. } => {
                self.app.handle_internal_event_with_render_impact(ev)
            }
            _ => {
                let changed = self.app.handle_internal_event_with_render_impact(ev);
                changed | self.finish_pending_client_pane_focus()
            }
        }
    }

    fn handle_client_owned_worktree_event(&mut self, client_id: u64, ev: AppEvent) -> bool {
        let Some(mut presentation) = self
            .clients
            .get_mut(&client_id)
            .map(|client| std::mem::take(&mut client.sidebar_presentation))
        else {
            return self.app.handle_internal_event_with_render_impact(ev);
        };
        self.app.state.swap_sidebar_presentation(&mut presentation);
        self.app.active_overlay_client_id = Some(client_id);
        let previous_mode = self.app.state.effective_interaction_mode();
        let changed = self.app.handle_internal_event_with_render_impact(ev);
        self.app.sync_prefix_input_source(previous_mode);
        self.app.take_pending_client_pane_focus();
        self.app.active_overlay_client_id = None;
        self.app.state.swap_sidebar_presentation(&mut presentation);
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.sidebar_presentation = presentation;
            client.request_repaint();
        }
        changed
    }

    fn refresh_client_loop_history_details(&mut self) {
        let history = self.app.state.loop_run_history.clone();
        for client in self.clients.values_mut() {
            let Some(detail) = client.loop_run_history_detail.as_mut() else {
                continue;
            };
            let loop_id = (detail.loop_id != crate::loop_runs::ALL_LOOPS_ID)
                .then_some(detail.loop_id.as_str());
            detail.history = crate::loop_runs::RunHistory {
                runs: crate::loop_runs::runs_for_loop(&history, loop_id),
                skipped_lines: history.skipped_lines,
            };
            detail.observed_at = std::time::SystemTime::now();
        }
    }

    fn refresh_client_work_views(&mut self) {
        let enabled = self.app.work_index_config.enabled;
        let snapshot = self.app.work_index_snapshot.clone();
        for client in self.clients.values_mut() {
            let Some(view) = client.work_view.as_mut() else {
                continue;
            };
            view.enabled = enabled;
            if !enabled {
                view.snapshot = None;
            } else if let Some(snapshot) = snapshot.clone() {
                view.replace_snapshot(snapshot);
            }
        }
    }

    fn refresh_client_usage_views(&mut self) {
        let snapshot = self.app.state.usage_snapshot.clone();
        for client in self.clients.values_mut() {
            let Some(view) = client.usage_view.as_mut() else {
                continue;
            };
            if let Some(snapshot) = snapshot.clone() {
                view.snapshot = Some(snapshot);
            }
            view.scanning = false;
        }
    }

    fn refresh_client_symphony_details(&mut self) {
        let snapshot = self.app.state.symphony_snapshot.clone();
        for client in self.clients.values_mut() {
            let Some(detail) = client.symphony_detail.as_mut() else {
                continue;
            };
            detail.replace_snapshot(snapshot.clone());
        }
    }

    /// Drains internal events, forwarding clipboard, sound, and toast
    /// notifications to connected clients instead of processing them locally.
    ///
    /// In the monolithic mode:
    /// - `ClipboardWrite` events are written to stdout via `write_osc52_bytes`.
    /// - Sound notifications are played locally via `sound::play`.
    /// - Toast notifications are set on AppState and rendered into the frame.
    ///
    /// In the headless server, there is no stdout terminal or audio subsystem,
    /// so we:
    /// - Forward `ClipboardWrite` as `ServerMessage::Clipboard` to the
    ///   foreground client only.
    /// - Detect when a sound would be played and forward as
    ///   `ServerMessage::Notify { kind: Sound }` to the foreground client.
    /// - Detect when a toast is set on AppState and forward as
    ///   `ServerMessage::Notify` to the foreground client for terminal/system delivery.
    fn drain_internal_events_with_forwarding(&mut self) -> bool {
        self.drain_internal_events_with_forwarding_up_to(crate::app::APP_EVENT_DRAIN_LIMIT)
            .1
    }

    fn drain_all_internal_events_with_forwarding(&mut self) -> bool {
        let mut changed = false;
        loop {
            let (had_event, batch_changed) =
                self.drain_internal_events_with_forwarding_up_to(crate::app::APP_EVENT_DRAIN_LIMIT);
            changed |= batch_changed;
            if !had_event || self.should_quit.load(Ordering::Acquire) {
                break;
            }
        }
        changed
    }

    fn drain_internal_events_with_forwarding_up_to(&mut self, limit: usize) -> (bool, bool) {
        let mut had_event = false;
        let mut changed = false;
        for _ in 0..limit {
            let Ok(ev) = self.app.event_rx.try_recv() else {
                break;
            };
            had_event = true;
            changed |= self.handle_internal_event_with_forwarding(ev);
        }
        (had_event, changed)
    }

    fn drain_client_config_reload_request(&mut self) {
        if !self.app.state.request_client_config_reload {
            return;
        }
        self.app.state.request_client_config_reload = false;
        self.send_to_all_clients(ServerMessage::ReloadSoundConfig);
        self.send_to_all_clients(ServerMessage::NotificationConfig {
            sound_enabled: self.app.state.sound.enabled,
        });
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
            self.send_client_graphics_cleanup(client_id);
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
        self.attach_terminal_client_with_control(client_id, terminal_id, takeover, None)
    }

    fn attach_terminal_client_with_control(
        &mut self,
        client_id: u64,
        terminal_id: String,
        takeover: bool,
        control: Option<Box<crate::server::remote_control::RemoteControlLease>>,
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
        let mut affected = client
            .terminal_geometries
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        client.terminal_geometries.clear();
        client.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: terminal_id.clone(),
            control,
        };
        client.pending_terminal_attach = false;
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
        affected.insert(real_terminal_id.clone());
        self.reconcile_terminal_geometries(affected);
        let (effective_rows, effective_cols) = self
            .app
            .terminal_runtimes
            .get(&real_terminal_id)
            .map_or((rows, cols), crate::terminal::TerminalRuntime::current_size);
        self.app.start_pending_agent_resume_for_terminal(
            &real_terminal_id,
            effective_rows,
            effective_cols,
            true,
        );
        true
    }

    fn client_is_pending_terminal_mode(&self, client_id: u64) -> bool {
        self.clients.get(&client_id).is_some_and(|client| {
            client.pending_terminal_attach && matches!(client.mode, ClientConnectionMode::App)
        })
    }

    #[cfg(unix)]
    fn controlled_remote_owners(
        &self,
    ) -> std::collections::HashMap<crate::terminal::TerminalId, u64> {
        self.clients
            .iter()
            .filter_map(|(client_id, client)| {
                let ClientConnectionMode::TerminalAttach {
                    control: Some(control),
                    ..
                } = &client.mode
                else {
                    return None;
                };
                self.terminal_id_by_string(&control.context.terminal_id)
                    .map(|terminal_id| (terminal_id, *client_id))
            })
            .collect()
    }

    fn route_full_app_human_events(
        &mut self,
        source_id: u64,
        events: Vec<crate::raw_input::RawInputEvent>,
        apply_host_terminal_theme: bool,
    ) -> bool {
        let mut pomodoro_presentation = self.pomodoro_input_presentation_for_input(source_id);
        #[cfg(unix)]
        {
            let controlled_owners = self.controlled_remote_owners();
            let mut human_controlled_owner = None;
            let mut before_terminal_input = |target: &crate::app::TerminalInputTarget| {
                if human_controlled_owner.is_none() {
                    human_controlled_owner = controlled_owners.get(target.terminal_id()).copied();
                }
            };
            let pomodoro_changed = self
                .app
                .route_client_events_from_with_human_input_hook_and_pomodoro_visibility(
                    source_id,
                    events,
                    apply_host_terminal_theme,
                    &mut before_terminal_input,
                    Some(&controlled_owners),
                    &mut pomodoro_presentation,
                );
            if let Some(owner_id) = human_controlled_owner {
                self.reject_remote_control(
                    owner_id,
                    crate::api::schema::ErrorBody {
                        code: "already_controlled".to_owned(),
                        message: "remote control ended by human input".to_owned(),
                    },
                );
            }
            if let Some(client) = self.clients.get_mut(&source_id) {
                client.pomodoro_presentation = pomodoro_presentation;
            }
            pomodoro_changed
        }
        #[cfg(not(unix))]
        {
            let mut before_terminal_input = |_: &crate::app::TerminalInputTarget| {};
            let pomodoro_changed = self
                .app
                .route_client_events_from_with_human_input_hook_and_pomodoro_visibility(
                    source_id,
                    events,
                    apply_host_terminal_theme,
                    &mut before_terminal_input,
                    None,
                    &mut pomodoro_presentation,
                );
            if let Some(client) = self.clients.get_mut(&source_id) {
                client.pomodoro_presentation = pomodoro_presentation;
            }
            pomodoro_changed
        }
    }

    /// Handles a server event. Returns true if the event requires a re-render.
    fn handle_client_input_events(
        &mut self,
        client_id: u64,
        events: Vec<crate::raw_input::RawInputEvent>,
    ) -> bool {
        let source_was_foreground = self.foreground_client_id == Some(client_id);
        let source_is_full_app = self
            .clients
            .get(&client_id)
            .is_some_and(ClientConnection::is_full_app_client);
        let source_policy = self.client_presentation_policy(client_id);
        let host_surface_redraw = crate::raw_input::events_require_host_surface_redraw(
            &events,
            self.app.state.redraw_on_focus_gained,
        );
        let hover_motion_changes = source_is_full_app
            && events.iter().any(|event| match event {
                crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::Moved,
                    column,
                    row,
                    ..
                }) => {
                    source_policy
                        .as_ref()
                        .is_some_and(|policy| policy.mouse_motion_changes_view())
                        || self.clients.get(&client_id).is_some_and(|client| {
                            client.dock_presentation.hovered_control.is_some()
                        })
                        || crate::ui::hovered_control_at(&self.app.state, *column, *row).is_some()
                }
                _ => false,
            });
        let source_mode = if source_is_full_app {
            source_policy
                .as_ref()
                .map(|policy| policy.mode())
                .unwrap_or_else(|| self.app.state.server_mode())
        } else {
            self.app.state.server_mode()
        };
        let render_neutral_mouse_motion =
            events_are_render_neutral_mouse_motion(&events, source_mode) && !hover_motion_changes;
        if let Some(client) = self.clients.get_mut(&client_id) {
            if host_surface_redraw {
                client.request_repaint();
                client.defer_full_render();
            } else if !render_neutral_mouse_motion {
                // Ensure semantic clients receive one post-input frame even if the
                // semantic buffer compares equal. Terminal-ANSI clients must keep their
                // server-side blit baseline; resetting it here forces a full redraw on
                // every keypress and makes remote sessions feel extremely slow.
                client.request_semantic_redraw_after_input();
            }
        }
        let mut pomodoro_changed = false;
        if source_is_full_app {
            pomodoro_changed = self.update_client_outer_focus_from_events(client_id, &events);
            if events
                .iter()
                .any(|event| matches!(event, crate::raw_input::RawInputEvent::OuterFocusLost))
            {
                // Focus loss is not a teardown, so the pending URL click stays.
                self.app.release_input_source_headless(client_id);
            }
        }
        let events = events_for_app_routing(events, source_was_foreground, source_is_full_app);
        let interaction = events_include_interaction(&events);
        let theme_changed = self.update_client_host_theme_from_events(client_id, &events);
        // Client-local theme reports were applied above; routing them again would update every
        // pane once per palette entry instead of once per captured batch.
        let mut input_presentation = if source_is_full_app {
            self.clients
                .get_mut(&client_id)
                .map(ClientInputPresentation::take)
        } else {
            None
        };
        if let Some(presentation) = &mut input_presentation {
            presentation.install(&mut self.app.state);
        }
        self.app.active_overlay_client_id = source_is_full_app.then_some(client_id);
        // Promotion and reconciliation are input transitions. They must see
        // this client's presentation, never whichever client rendered last.
        let foreground_changed = if interaction {
            source_policy.as_ref().is_some_and(|policy| {
                self.promote_client_to_foreground_with_policy(client_id, policy)
            })
        } else {
            false
        };
        let input_geometry_dirty = source_is_full_app
            && self
                .clients
                .get(&client_id)
                .is_some_and(|client| client.input_geometry_dirty);
        if foreground_changed || input_geometry_dirty {
            // Promotion changes the effective size, but input geometry also
            // depends on the source client's attach-local presentation. A
            // resize computes shared runtime geometry before that presentation
            // is swapped in, so the first input after it must refresh hit areas.
            self.resize_shared_runtime_to_effective_size_before_input();
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.input_geometry_dirty = false;
            }
        }
        let interaction_reconciled = self
            .app
            .reconcile_client_interaction(interaction && source_is_full_app);
        pomodoro_changed |= self.route_full_app_human_events(client_id, events, false);
        self.app.take_pending_client_pane_focus();
        if source_is_full_app {
            pomodoro_changed |= self.handle_client_overlay_deferred_requests();
        }
        self.app.active_overlay_client_id = None;
        if pomodoro_changed || interaction_reconciled {
            // A modal dismissal is a presentation change even for a plain
            // mouse move, which is otherwise intentionally render-neutral.
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.request_repaint();
            }
        }
        self.app.start_usage_scan_if_requested();
        if let Some(mut presentation) = input_presentation {
            presentation.uninstall(&mut self.app.state);
            if let Some(client) = self.clients.get_mut(&client_id) {
                presentation.store(client);
            }
        }
        if self.app.take_config_reloaded_from_disk() {
            self.reload_server_config(false);
            self.refresh_client_work_views();
        } else {
            self.sync_foreground_client_state();
        }

        if let Some(sound_enabled) = self.app.state.request_client_notification_config.take() {
            self.send_to_client(
                client_id,
                ServerMessage::NotificationConfig { sound_enabled },
            );
        }

        if let Some(width) = self.app.state.take_dock_width_persistence_request() {
            self.send_to_client(client_id, ServerMessage::DockWidth { width });
        }
        if let Some(height) = self.app.state.take_notepad_height_persistence_request() {
            crate::client::presentation::save_notepad_height(height);
        }
        if let Some(mode) = self.app.state.take_sidebar_group_mode_persistence_request() {
            crate::client::presentation::save_sidebar_group_mode(mode);
        }
        if let Some((key, mode)) = self.app.state.take_sidebar_group_sort_persistence_request() {
            crate::client::presentation::save_sidebar_group_sort(&key, mode);
        }
        if let Some((key, collapsed)) = self
            .app
            .state
            .take_sidebar_group_collapsed_persistence_request()
        {
            crate::client::presentation::save_sidebar_group_collapsed(&key, collapsed);
        }
        if self.app.state.take_sidebar_view_scan_request() {
            self.app.request_sidebar_view_scan(Instant::now());
        }
        if let Some(filter) = self
            .app
            .state
            .take_sidebar_work_filter_persistence_request()
        {
            crate::client::presentation::save_sidebar_work_filter(filter);
        }

        if self.app.state.detach_requested {
            self.app.state.detach_requested = false;
            info!(client_id, "client detach requested via keybind");

            self.send_client_graphics_cleanup(client_id);
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some("detached".to_owned()),
                },
            );

            if let Some(client) = self.clients.get_mut(&client_id) {
                client.writer = None;
            }

            false
        } else {
            pomodoro_changed
                || foreground_changed
                || theme_changed
                || (interaction && !render_neutral_mouse_motion)
        }
    }

    fn handle_server_event(&mut self, ev: ServerEvent) -> bool {
        if self.handoff_in_progress && Self::ignore_client_event_during_handoff(&ev) {
            return false;
        }

        let stale_client_id = match &ev {
            ServerEvent::ClientConnected { .. } | ServerEvent::QuitSignal => None,
            ServerEvent::ClientInput { client_id, .. }
            | ServerEvent::GraphicsTransmissionResult { client_id, .. }
            | ServerEvent::GraphicsTransmissionStarted { client_id, .. }
            | ServerEvent::ClientInputPixels { client_id, .. }
            | ServerEvent::ClientInputEvents { client_id, .. }
            | ServerEvent::ClientDockWidth { client_id, .. }
            | ServerEvent::ClientPasteRejected { client_id, .. }
            | ServerEvent::ClientClipboardImage { client_id, .. }
            | ServerEvent::ClientAttachTerminal { client_id, .. }
            | ServerEvent::ClientObserveTerminal { client_id, .. }
            | ServerEvent::ClientControlTerminal { client_id, .. }
            | ServerEvent::ClientAttachScroll { client_id, .. }
            | ServerEvent::ClientResize { client_id, .. }
            | ServerEvent::ClientDetach { client_id }
            | ServerEvent::ClientDisconnected { client_id }
            | ServerEvent::ClientWriterDrained { client_id, .. } => Some(*client_id),
        };
        if stale_client_id.is_some_and(|client_id| !self.clients.contains_key(&client_id)) {
            return false;
        }

        match ev {
            ServerEvent::ClientConnected {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                keybindings,
                writer,
                render_encoding,
                direct_attach_requested,
                direct_graphics,
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
                let first_app_client = !direct_attach_requested && self.app_client_count() == 0;
                let attach_now = Instant::now();
                info!(
                    client_id,
                    cols,
                    rows,
                    cell_width_px,
                    cell_height_px,
                    ?render_encoding,
                    "client connected"
                );
                let last_activity = self.allocate_activity_stamp();
                let mut connection = ClientConnection::new_with_mode(
                    ClientConnectionMode::App,
                    keybindings,
                    (cols, rows),
                    crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    },
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    last_activity,
                    render_encoding,
                    direct_attach_requested,
                    Some(writer),
                );
                connection.direct_graphics = direct_graphics;
                connection.pixel_mouse = direct_graphics;
                self.clients.insert(client_id, connection);
                if first_app_client {
                    self.app.tick_pomodoro(attach_now, false);
                }
                if !direct_attach_requested && self.app_clients_host_focused() {
                    self.app.state.pomodoro.resume_held(attach_now);
                }
                self.seed_client_dock_presentation(client_id);
                if let Some(client) = self.clients.get_mut(&client_id) {
                    let group_mode = crate::client::presentation::load_sidebar_group_mode();
                    let collapsed = crate::client::presentation::load_sidebar_group_collapsed();
                    client
                        .sidebar_presentation
                        .initialize_group_mode(group_mode, &collapsed);
                    client.sidebar_presentation.work_filter =
                        crate::client::presentation::load_sidebar_work_filter();
                    client.sidebar_presentation.group_sorts =
                        crate::client::presentation::load_sidebar_group_sorts();
                }
                let foreground_changed =
                    !direct_attach_requested && self.foreground_client_id != Some(client_id);
                if !direct_attach_requested {
                    self.send_to_client(
                        client_id,
                        ServerMessage::NotificationConfig {
                            sound_enabled: self.app.state.sound.enabled,
                        },
                    );
                    self.foreground_client_id = Some(client_id);
                }
                if first_app_client {
                    self.app.mark_git_status_refresh_due(Instant::now());
                    self.last_app_client_seen = Instant::now();
                    self.app.next_work_index_refresh = Instant::now();
                }
                self.sync_foreground_client_state();
                if foreground_changed {
                    if let Some(policy) = self.client_presentation_policy(client_id) {
                        self.sync_client_input_source(client_id, &policy);
                    }
                }
                self.resize_shared_runtime_to_effective_size();
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
                agent_ref,
                expected_context,
                takeover,
            } => self.control_terminal_client(
                client_id,
                target,
                agent_ref,
                expected_context,
                takeover,
            ),
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
            ServerEvent::ClientInputPixels {
                client_id,
                data,
                geometry,
            } => {
                if !self.clients.contains_key(&client_id) {
                    return false;
                }
                let coordinates_valid = crate::input::mouse::parse_report(&data)
                    .and_then(|(x, y)| geometry.cell(x, y))
                    .is_some();
                let valid = coordinates_valid
                    && self.clients.get(&client_id).is_some_and(|client| {
                        let cell = client.cell_size;
                        client.is_full_app_client()
                            && client.host_sgr_pixels_active == Some(true)
                            && client.terminal_size == (geometry.cols, geometry.rows)
                            && cell.is_known()
                            && cell.width_px == geometry.width_px / u32::from(geometry.cols)
                            && cell.height_px == geometry.height_px / u32::from(geometry.rows)
                    });
                if !valid || self.handoff_in_progress || !self.focused_pane_graphics_demand() {
                    return false;
                }
                let Some((x, y)) = crate::input::mouse::parse_report(&data) else {
                    return false;
                };
                let Some((column, row)) = geometry.cell(x, y) else {
                    return false;
                };
                let Some(cell_report) = crate::input::mouse::report_at_cell(&data, column, row)
                else {
                    return false;
                };
                let events = crate::raw_input::parse_raw_input_bytes_sync(&cell_report);
                if events.len() != 1
                    || !matches!(events[0], crate::raw_input::RawInputEvent::Mouse(_))
                {
                    return false;
                }
                self.app.state.host_mouse_pixels =
                    Some(crate::input::mouse::HostPixels { x, y, geometry });
                let changed = self.handle_client_input_events(client_id, events);
                self.app.state.host_mouse_pixels = None;
                changed
            }
            ServerEvent::ClientInput { client_id, data } => {
                if !self.clients.contains_key(&client_id) {
                    return false;
                }
                if self.handoff_in_progress {
                    debug!(
                        client_id,
                        len = data.len(),
                        "ignored client input during handoff"
                    );
                    return false;
                }
                if self
                    .clients
                    .get(&client_id)
                    .is_some_and(|client| client.pending_terminal_attach)
                {
                    debug!(
                        client_id,
                        len = data.len(),
                        "ignored client input while terminal control is connecting"
                    );
                    return false;
                }
                debug!(client_id, len = data.len(), "client input received");
                let attached_terminal = self.clients.get(&client_id).and_then(|client| {
                    if let ClientConnectionMode::TerminalAttach {
                        terminal_id,
                        control,
                    } = &client.mode
                    {
                        Some((terminal_id.clone(), control.is_some()))
                    } else {
                        None
                    }
                });
                if let Some((_terminal_id, true)) = attached_terminal {
                    return self.forward_control_bytes(client_id, data);
                }
                if let Some((terminal_id, false)) = attached_terminal {
                    if let Some(Err(err)) =
                        self.forward_terminal_attach_bytes(&terminal_id, data, true)
                    {
                        warn!(client_id, terminal_id = %terminal_id, err = %err);
                    }
                    return true;
                }
                if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalObserve { .. })
                ) {
                    return false;
                }
                let events = if let Some(client) = self.clients.get_mut(&client_id) {
                    let mut events = client.raw_input.push(&data);
                    // The thin client only forwards a bare ESC after its local input timeout.
                    if data.as_slice() == b"\x1b" {
                        events.extend(client.raw_input.flush_timeout());
                    }
                    events
                } else {
                    Vec::new()
                };
                self.handle_client_input_events(client_id, events)
            }
            ServerEvent::ClientInputEvents { client_id, events } => {
                if !self.clients.contains_key(&client_id) {
                    return false;
                }
                if self.handoff_in_progress {
                    debug!(
                        client_id,
                        len = events.len(),
                        "ignored client input events during handoff"
                    );
                    return false;
                }
                if self
                    .clients
                    .get(&client_id)
                    .is_some_and(|client| client.pending_terminal_attach)
                {
                    debug!(
                        client_id,
                        len = events.len(),
                        "ignored structured input while terminal control is connecting"
                    );
                    return false;
                }
                debug!(
                    client_id,
                    len = events.len(),
                    "client input events received"
                );
                if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalObserve { .. })
                ) {
                    return false;
                }
                if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalAttach {
                        control: Some(_),
                        ..
                    })
                ) {
                    return false;
                }
                let events = events
                    .iter()
                    .map(crate::protocol::ClientInputEvent::to_raw_input_event)
                    .collect();
                self.handle_client_input_events(client_id, events)
            }
            ServerEvent::ClientDockWidth { client_id, width } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                if !client.is_full_app_client() {
                    return false;
                }
                client.dock_presentation.width =
                    width.clamp(crate::ui::DOCK_MIN_WIDTH, crate::ui::DOCK_MAX_WIDTH);
                true
            }
            ServerEvent::ClientPasteRejected {
                client_id,
                size,
                max,
            } => {
                self.send_to_client(
                    client_id,
                    ServerMessage::Notify {
                        kind: protocol::NotifyKind::Toast,
                        message: "Paste rejected".to_owned(),
                        body: Some(format!(
                            "Input message is {size} bytes; Herdr's limit is {max} bytes"
                        )),
                    },
                );
                false
            }
            ServerEvent::ClientClipboardImage {
                client_id,
                extension,
                data,
            } => {
                if !self.clients.contains_key(&client_id) {
                    return false;
                }
                debug!(
                    client_id,
                    len = data.len(),
                    extension = %extension,
                    "client clipboard image received"
                );
                if self
                    .clients
                    .get(&client_id)
                    .is_some_and(|client| client.pending_terminal_attach)
                {
                    debug!(
                        client_id,
                        "ignored clipboard input while terminal control is connecting"
                    );
                    return false;
                }
                if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalObserve { .. })
                ) {
                    return false;
                }
                match self.write_client_clipboard_image(client_id, &extension, &data) {
                    Ok(path) => self.paste_client_clipboard_image_path(client_id, path),
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
            } => {
                info!(
                    client_id,
                    cols, rows, cell_width_px, cell_height_px, "client resize"
                );
                let controlled_terminal_id = if let Some(ClientConnection {
                    mode:
                        ClientConnectionMode::TerminalAttach {
                            terminal_id,
                            control: Some(_),
                        },
                    terminal_size,
                    cell_size,
                    render_state,
                    ..
                }) = self.clients.get_mut(&client_id)
                {
                    *terminal_size = (cols, rows);
                    let observed = crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    };
                    if observed.is_known() {
                        *cell_size = observed;
                    }
                    render_state.request_repaint();
                    Some(terminal_id.clone())
                } else {
                    None
                };
                if let Some(terminal_id) = controlled_terminal_id {
                    if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
                        self.reconcile_terminal_geometries([terminal_id]);
                    }
                    return true;
                }
                let direct_terminal_id = if let Some(ClientConnection {
                    mode:
                        ClientConnectionMode::TerminalAttach {
                            terminal_id,
                            control: None,
                        },
                    terminal_size,
                    cell_size,
                    render_state,
                    ..
                }) = self.clients.get_mut(&client_id)
                {
                    *terminal_size = (cols, rows);
                    let observed = crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    };
                    if observed.is_known() {
                        *cell_size = observed;
                    }
                    render_state.request_repaint();
                    Some(terminal_id.clone())
                } else {
                    None
                };
                if let Some(terminal_id) = direct_terminal_id {
                    if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
                        self.reconcile_terminal_geometries([terminal_id]);
                    }
                    return true;
                }
                let observed_terminal_id = if let Some(ClientConnection {
                    mode: ClientConnectionMode::TerminalObserve { terminal_id },
                    terminal_size,
                    cell_size,
                    render_state,
                    ..
                }) = self.clients.get_mut(&client_id)
                {
                    *terminal_size = (cols, rows);
                    let observed = crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    };
                    if observed.is_known() {
                        *cell_size = observed;
                    }
                    render_state.request_repaint();
                    Some(terminal_id.clone())
                } else {
                    None
                };
                if let Some(terminal_id) = observed_terminal_id {
                    if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
                        self.reconcile_terminal_geometries([terminal_id]);
                    }
                    return true;
                }
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.terminal_size = (cols, rows);
                    client.input_geometry_dirty = true;
                    let observed = crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    };
                    if observed.is_known() {
                        client.cell_size = observed;
                    }
                }
                self.promote_client_to_foreground(client_id);
                self.resize_shared_runtime_to_effective_size();
                true
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
            ServerEvent::ClientWriterDrained {
                client_id,
                render_sequence,
            } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                if let Some(presentation) =
                    client.take_flushed_pomodoro_presentation(render_sequence)
                {
                    client.pomodoro_presentation = crate::ui::pomodoro::input_gate_at(
                        &self.app.state,
                        presentation,
                        Instant::now(),
                    );
                }
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
            || terminal.raw_agent_state() == crate::detect::AgentState::Idle
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
        let status = crate::detect::manifest::agent_state_label(terminal.raw_agent_state());
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
            || terminal.raw_agent_state() != crate::detect::AgentState::Idle
        {
            return None;
        }
        let runtime = self.app.terminal_runtimes.get(&terminal.id)?;
        if runtime.wheel_routing() != Some(crate::pane::WheelRouting::MouseReport) {
            return None;
        }
        let (screen, initial) = runtime.screen_text_snapshot()?;
        if screen != crate::ghostty::ActiveScreen::Alternate || initial.rows.len() >= lines {
            return None;
        }
        Some(AltScreenReadSpec {
            terminal_id: terminal.id.clone(),
            lines,
            unwrap: source == ReadSource::RecentUnwrapped,
            initial,
        })
    }

    fn poll_pending_alt_screen_reads(&mut self, now: Instant) {
        let pending = std::mem::take(&mut self.pending_alt_screen_reads);
        let mut failed_restores = Vec::new();
        for read in pending {
            let terminal_id = read.terminal_id.clone();
            let runtime = self.app.terminal_runtimes.get(&read.terminal_id);
            let remains_idle =
                self.app
                    .state
                    .terminals
                    .get(&read.terminal_id)
                    .is_some_and(|terminal| {
                        terminal.raw_agent_state() == crate::detect::AgentState::Idle
                    });
            let attached = self
                .terminal_attach_owners
                .contains_key(read.terminal_id.as_str());
            let outcome = if remains_idle && !attached {
                read.poll(runtime, now)
            } else {
                read.abort(runtime, now)
            };
            match outcome {
                crate::server::alt_screen_read::PollOutcome::Pending(read) => {
                    self.pending_alt_screen_reads.push(*read);
                }
                crate::server::alt_screen_read::PollOutcome::Complete {
                    viewport_restored: false,
                } => failed_restores.push(terminal_id),
                crate::server::alt_screen_read::PollOutcome::Complete {
                    viewport_restored: true,
                } => {}
            }
        }
        for terminal_id in failed_restores {
            self.fail_deferred_alt_screen_reads(&terminal_id);
        }
    }

    fn pending_alt_screen_read_index(&self, request: &api::schema::Request) -> Option<usize> {
        let target = match &request.method {
            api::schema::Method::AgentRead(params) => {
                self.app.resolve_agent_target(&params.target).ok()
            }
            api::schema::Method::PaneRead(params) => {
                self.app.resolve_terminal_target(&params.pane_id).ok()
            }
            _ => return None,
        };
        let target = target?;
        self.pending_alt_screen_reads
            .iter()
            .position(|pending| pending.terminal_id.as_str() == target.terminal_id)
    }

    fn cancel_alt_screen_read_conflict(
        &mut self,
        request: &api::schema::Request,
        now: Instant,
    ) -> AltScreenReadConflict {
        let Some(index) = self.pending_alt_screen_read_index(request) else {
            return AltScreenReadConflict::None;
        };
        let read = self.pending_alt_screen_reads.remove(index);
        let runtime = self.app.terminal_runtimes.get(&read.terminal_id);
        match read.abort(runtime, now) {
            crate::server::alt_screen_read::PollOutcome::Pending(read) => {
                self.pending_alt_screen_reads.push(*read);
                AltScreenReadConflict::Defer
            }
            crate::server::alt_screen_read::PollOutcome::Complete {
                viewport_restored: true,
            } => AltScreenReadConflict::Cancelled,
            crate::server::alt_screen_read::PollOutcome::Complete {
                viewport_restored: false,
            } => AltScreenReadConflict::RestoreFailed,
        }
    }

    fn fail_deferred_alt_screen_reads(&mut self, terminal_id: &crate::terminal::TerminalId) {
        let deferred = std::mem::take(&mut self.deferred_alt_screen_reads);
        for msg in deferred {
            let targets_terminal = match &msg.request.method {
                api::schema::Method::AgentRead(params) => self
                    .app
                    .resolve_agent_target(&params.target)
                    .ok()
                    .is_some_and(|target| target.terminal_id == terminal_id.as_str()),
                api::schema::Method::PaneRead(params) => self
                    .app
                    .resolve_terminal_target(&params.pane_id)
                    .ok()
                    .is_some_and(|target| target.terminal_id == terminal_id.as_str()),
                _ => false,
            };
            if targets_terminal {
                let response = alt_screen_restore_error_response(msg.request.id);
                let _ = msg.respond_to.send(response);
            } else {
                self.deferred_alt_screen_reads.push(msg);
            }
        }
    }

    fn process_deferred_alt_screen_reads(&mut self) -> bool {
        let deferred = std::mem::take(&mut self.deferred_alt_screen_reads);
        let mut changed = false;
        for msg in deferred {
            if self.pending_alt_screen_read_index(&msg.request).is_some() {
                self.deferred_alt_screen_reads.push(msg);
            } else {
                changed |= self.handle_api_request_with_shutdown_check_inner(msg, false, true);
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

    /// Handles a single API request with shutdown awareness.
    ///
    /// Also forwards any toast/sound notifications that result from the API
    /// request to connected clients. API methods like `pane.report_agent`
    /// trigger internal events that may set toast state or would normally
    /// play sounds — in headless mode we forward these to clients instead.
    fn handle_api_request_with_shutdown_check(&mut self, msg: api::ApiRequestMessage) -> bool {
        self.handle_api_request_with_shutdown_check_inner(msg, false, false)
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
        if self.handle_api_request_with_shutdown_check_inner(msg, false, false) {
            RenderImpact::Full
        } else {
            RenderImpact::None
        }
    }

    fn handle_api_request_with_shutdown_check_inner(
        &mut self,
        msg: api::ApiRequestMessage,
        skip_default_workspace_for_request: bool,
        skip_alt_screen_capture: bool,
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

        let skip_alt_screen_capture =
            match self.cancel_alt_screen_read_conflict(&msg.request, Instant::now()) {
                AltScreenReadConflict::None => skip_alt_screen_capture,
                AltScreenReadConflict::Cancelled => true,
                AltScreenReadConflict::Defer => {
                    self.deferred_alt_screen_reads.push(msg);
                    return false;
                }
                AltScreenReadConflict::RestoreFailed => {
                    let response = alt_screen_restore_error_response(msg.request.id);
                    let _ = msg.respond_to.send(response);
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
            let (response, handoff_succeeded) = live_handoff_api_response(
                msg.request.id,
                self.perform_live_handoff(params.clone()),
            );
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
                                    terminal.raw_agent_state(),
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
        let alt_screen_read_spec = (!skip_alt_screen_capture)
            .then(|| self.alt_screen_read_spec(&msg.request))
            .flatten();
        if matches!(
            &msg.request.method,
            api::schema::Method::WorktreeCreate(_) | api::schema::Method::WorktreeRemove(_)
        ) {
            let deferred_changed = self
                .app
                .handle_deferred_worktree_api_request(msg.request, msg.respond_to);
            return changed | deferred_changed;
        }
        if self.app.should_defer_group_api_request(&msg.request) {
            self.app
                .start_deferred_group_api_request(msg.request, msg.respond_to);
            return changed;
        }
        let (response, api_pane_state_updates) = if matches!(
            &msg.request.method,
            api::schema::Method::ServerReloadConfig(_)
        ) {
            let report = self.reload_server_config(true);
            let response = serde_json::to_string(&api::schema::SuccessResponse {
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
            });
            (response, Vec::new())
        } else {
            self.app
                .handle_api_request_after_internal_events_drained_with_pane_updates(msg.request)
        };
        changed |= self.finish_pending_client_pane_focus();
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

            let Some(terminal_after) = self
                .app
                .state
                .terminals
                .get(&pane_after.attached_terminal_id)
            else {
                continue;
            };

            let new_state = terminal_after.raw_agent_state();
            if new_state == *prev_state {
                continue;
            }
            let suppress_completion = api_pane_state_updates.iter().any(|update| {
                update.pane_id == *pane_id
                    && update.previous_state == *prev_state
                    && update.state == new_state
                    && update.suppress_completion
            });

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

            if !suppress_completion
                && !forwarded_toast_from_state
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
                        .get(&pane_after.attached_terminal_id)
                        .and_then(|terminal| terminal.effective_agent_label())
                    {
                        let event_text = match kind {
                            crate::app::state::ToastKind::NeedsAttention => "needs attention",
                            crate::app::state::ToastKind::Finished => "finished",
                            crate::app::state::ToastKind::UpdateInstalled => "updated",
                            crate::app::state::ToastKind::WorkLinked => "linked",
                        };
                        let workspace_label = self.app.state.workspaces[*ws_idx].display_name_from(
                            &self.app.state.terminals,
                            &self.app.terminal_runtimes,
                        );
                        let context = crate::app::actions::notification_context(
                            &self.app.state.workspaces[*ws_idx],
                            &self.app.state.terminals,
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
            if !suppress_completion
                && self.app.state.toast_config.delay_seconds == 0
                && self.app.state.sound.allows(agent)
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

        if !skip_default_workspace && latest_app_client(&self.clients).is_some() {
            changed |= self.app.ensure_default_workspace();
        }

        changed
    }

    fn focused_pane_graphics_demand(&self) -> bool {
        self.app
            .state
            .active
            .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
            .and_then(crate::workspace::Workspace::focused_pane_id)
            .is_some_and(|pane_id| self.app.pane_graphics.active_for_pane(pane_id))
    }

    fn stream_host_mouse_capture_mode(&mut self) {
        let enabled = self
            .app
            .state
            .should_capture_host_mouse_from(&self.app.terminal_runtimes);
        let pixel_mouse_requested = self
            .clients
            .values()
            .any(|client| client.is_full_app_client() && client.pixel_mouse);
        let sgr_pixels = pixel_mouse_requested
            && self.focused_pane_graphics_demand()
            && self
                .app
                .state
                .active
                .and_then(|ws_idx| {
                    self.app
                        .state
                        .workspaces
                        .get(ws_idx)
                        .and_then(crate::workspace::Workspace::focused_pane_id)
                        .and_then(|pane_id| {
                            self.app.state.runtime_for_pane_in_workspace(
                                &self.app.terminal_runtimes,
                                ws_idx,
                                pane_id,
                            )
                        })
                })
                .is_some_and(crate::terminal::TerminalRuntime::sgr_pixel_mouse_enabled);
        let mut broken_clients: Vec<u64> = Vec::new();
        for (&client_id, client) in &mut self.clients {
            if !client.is_full_app_client() {
                continue;
            }
            let client_sgr_pixels = sgr_pixels && client.pixel_mouse;
            if client.host_mouse_capture_active == Some(enabled)
                && client.host_sgr_pixels_active == Some(client_sgr_pixels)
            {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized = match Self::frame_server_message(&ServerMessage::MouseCapture {
                enabled,
                sgr_pixels: client_sgr_pixels,
            }) {
                Ok(framed) => framed,
                Err(err) => {
                    warn!(err = %err, "failed to serialize mouse capture mode for client");
                    continue;
                }
            };
            if writer.control.send(serialized).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during mouse capture update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_mouse_capture_active = Some(enabled);
            client.host_sgr_pixels_active = Some(client_sgr_pixels);
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    fn stream_host_keyboard_enhancement_flags(&mut self) {
        let report_all_keys = self.app.host_keyboard_report_all_requested();
        let serialized = match Self::frame_server_message(&ServerMessage::KittyKeyboardReportAll {
            enabled: report_all_keys,
        }) {
            Ok(framed) => framed,
            Err(err) => {
                warn!(err = %err, "failed to serialize keyboard enhancement flags for clients");
                return;
            }
        };

        let mut broken_clients = Vec::new();
        for (&client_id, client) in &mut self.clients {
            if !client.is_full_app_client()
                || client.host_keyboard_report_all_active == Some(report_all_keys)
            {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            if writer.control.send(serialized.clone()).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during keyboard enhancement update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_keyboard_report_all_active = Some(report_all_keys);
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    fn has_pending_presentation_work(
        &self,
        needs_full_render: bool,
        needs_graphics_render: bool,
    ) -> bool {
        needs_full_render || needs_graphics_render || self.app.render_dirty.has_immediate_work()
    }

    fn sync_immediate_pty_sources(&self) {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        let mut pane_ids = if has_app_target {
            self.app
                .state
                .app_surface_pane_ids_with_tab_visibility(self.any_app_client_displays_tab())
        } else {
            HashSet::new()
        };
        if !direct_terminal_targets.is_empty() {
            for workspace in &self.app.state.workspaces {
                for tab in &workspace.tabs {
                    pane_ids.extend(tab.panes.iter().filter_map(|(&pane_id, pane)| {
                        direct_terminal_targets
                            .contains(pane.attached_terminal_id.as_str())
                            .then_some(pane_id)
                    }));
                }
            }
            if let Some(popup) = &self.app.state.popup_pane {
                if direct_terminal_targets.contains(popup.terminal_id.as_str()) {
                    pane_ids.insert(popup.pane_id);
                }
            }
        }
        self.app.render_dirty.set_immediate_pty_sources(pane_ids);
    }

    fn pty_render_targets(&self) -> (bool, HashSet<&str>) {
        let mut has_app_target = false;
        let mut direct_terminal_targets = HashSet::new();
        for client in self
            .clients
            .values()
            .filter(|client| client.writer.is_some())
        {
            match &client.mode {
                ClientConnectionMode::App if client.is_full_app_client() => {
                    has_app_target = true;
                }
                ClientConnectionMode::TerminalAttach { terminal_id, .. }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    direct_terminal_targets.insert(terminal_id.as_str());
                }
                ClientConnectionMode::App => {}
            }
        }
        (has_app_target, direct_terminal_targets)
    }

    fn any_app_client_displays_tab(&self) -> bool {
        self.clients.values().any(|client| {
            client.writer.is_some()
                && client.is_full_app_client()
                && client
                    .presentation_policy(&self.app.state)
                    .tab_surface_visible()
        })
    }

    fn pty_source_visible_to_render_targets(
        &self,
        pane_id: crate::layout::PaneId,
        has_app_target: bool,
        direct_terminal_targets: &HashSet<&str>,
    ) -> bool {
        let terminal_id = self.terminal_id_for_pane(pane_id);
        (has_app_target && (terminal_id.is_none() || self.app_surface_contains_pane(pane_id)))
            || terminal_id.is_none_or(|source| direct_terminal_targets.contains(source.as_str()))
    }

    fn pty_sources_visible_to_any_render_target(
        &self,
        sources: &HashSet<crate::layout::PaneId>,
    ) -> bool {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        if !has_app_target && direct_terminal_targets.is_empty() {
            return false;
        }

        sources.iter().copied().any(|pane_id| {
            self.pty_source_visible_to_render_targets(
                pane_id,
                has_app_target,
                &direct_terminal_targets,
            )
        })
    }

    fn terminal_id_for_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalId> {
        if let Some(popup) = self
            .app
            .state
            .popup_pane
            .as_ref()
            .filter(|popup| popup.pane_id == pane_id)
        {
            return Some(&popup.terminal_id);
        }
        self.app
            .find_pane(pane_id)
            .map(|(_, pane)| &pane.attached_terminal_id)
    }

    fn app_surface_contains_pane(&self, pane_id: crate::layout::PaneId) -> bool {
        if self
            .app
            .state
            .popup_pane
            .as_ref()
            .is_some_and(|popup| popup.pane_id == pane_id)
        {
            return true;
        }
        if !self.any_app_client_displays_tab() {
            return false;
        }
        let Some(workspace) = self
            .app
            .state
            .active
            .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
        else {
            return false;
        };
        let Some(tab) = workspace.active_tab() else {
            return false;
        };
        if !tab.panes.contains_key(&pane_id) {
            return false;
        }
        !tab.zoomed || tab.layout.focused() == pane_id
    }

    fn render_retained_pty_update_and_stream(&mut self) -> bool {
        crate::render_prof::event("retained.attempt");
        let retained_started = crate::render_prof::timer();
        macro_rules! retained_fallback {
            ($reason:literal) => {{
                crate::render_prof::event(concat!("retained_fallback.", $reason));
                crate::render_prof::duration_since("retained.total", retained_started);
                return false;
            }};
        }
        macro_rules! retained_success {
            ($reason:literal) => {{
                crate::render_prof::event("retained.success");
                crate::render_prof::event(concat!("retained_success.", $reason));
                crate::render_prof::duration_since("retained.total", retained_started);
                return true;
            }};
        }

        if !self.retained_pty_update_allowed_by_app_state() {
            retained_fallback!("unsafe_app_state");
        }

        let render_targets = render_targets(&self.clients, self.foreground_client_id);
        let mut retained_target = None;
        let mut app_target_count = 0;
        for (client_id, size, cell_size, _is_foreground, mode) in &render_targets {
            if !matches!(mode, ClientConnectionMode::App) {
                retained_fallback!("not_app_client");
            }
            app_target_count += 1;
            let Some(client) = self.clients.get(client_id) else {
                retained_fallback!("client_missing");
            };
            if !client
                .presentation_policy(&self.app.state)
                .tab_surface_visible()
            {
                continue;
            }
            if retained_target.is_some() {
                retained_fallback!("multiple_tab_targets");
            }
            retained_target = Some((*client_id, *size, *cell_size));
        }
        let Some((client_id, (cols, rows), cell_size)) = retained_target else {
            if app_target_count > 0 {
                retained_success!("tab_surface_replaced");
            }
            retained_fallback!("no_target");
        };
        if self
            .client_presentation_policy(client_id)
            .is_some_and(|policy| policy.owns_input())
        {
            retained_fallback!("client_overlay");
        }
        let Some(client) = self.clients.get(&client_id) else {
            retained_fallback!("client_missing");
        };
        if client.deferred_render() != DeferredRender::None {
            retained_fallback!("render_pending");
        }
        if self.app.state.kitty_graphics_enabled && !client.graphics_cache.is_empty() {
            retained_fallback!("graphics_cache_active");
        }
        if client.graphics_surface_reset_pending {
            retained_fallback!("graphics_surface_reset");
        }
        if self.app.state.kitty_graphics_enabled
            && cell_size.is_known()
            && crate::kitty_graphics::has_visible_pane_graphics(
                &self.app.state,
                &self.app.pane_graphics,
                &self.app.terminal_runtimes,
                crate::ui::TabSurfaceView {
                    pane_infos: client.retained_pane_infos.as_slice(),
                    split_borders: &[],
                },
                cell_size,
            )
        {
            retained_fallback!("visible_kitty_graphics");
        }
        let Some(mut frame) = client.render_state.last_frame().cloned() else {
            retained_fallback!("no_last_frame");
        };
        if frame.width != cols || frame.height != rows {
            retained_fallback!("frame_size_mismatch");
        }
        frame.graphics.clear();

        let Some(ws_idx) = self.app.state.active else {
            retained_fallback!("no_active_workspace");
        };
        let pane_infos = client.retained_pane_infos.clone();
        let retained_pane_cursor = client.retained_pane_cursor;
        if pane_infos.is_empty() {
            retained_fallback!("no_pane_info");
        }

        let mut touched = false;
        for info in pane_infos.iter() {
            if !rect_fits_frame(info.inner_rect, &frame) {
                retained_fallback!("pane_rect_outside_frame");
            }
            let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                ws_idx,
                info.id,
            ) else {
                retained_fallback!("missing_runtime");
            };
            match runtime.collect_dirty_patch(info.inner_rect.width, info.inner_rect.height) {
                crate::pane::TerminalDirtyPatchOutcome::Clean => {
                    crate::render_prof::event("retained.pane_clean");
                }
                crate::pane::TerminalDirtyPatchOutcome::Fallback => {
                    retained_fallback!("dirty_patch_fallback");
                }
                crate::pane::TerminalDirtyPatchOutcome::Patch(patch) => {
                    crate::render_prof::event("retained.pane_patch");
                    crate::render_prof::counter("retained.patch_rows", patch.rows.len() as u64);
                    if dirty_patch_intersects_hyperlinks(&frame, info.inner_rect, &patch) {
                        retained_fallback!("hyperlink_intersection");
                    }
                    if !apply_terminal_dirty_patch(&mut frame, info.inner_rect, patch) {
                        retained_fallback!("patch_apply_failed");
                    }
                    touched = true;
                }
            }
        }

        let previous_cursor = frame.cursor.clone();
        if retained_pane_cursor {
            frame.cursor = crate::ui::tab_surface_cursor(
                &self.app.state,
                &self.app.terminal_runtimes,
                crate::ui::TabSurfaceView {
                    pane_infos: &pane_infos,
                    split_borders: &[],
                },
            );
        }
        let cursor_changed = frame.cursor != previous_cursor;

        if !touched && !cursor_changed {
            retained_success!("clean_no_cursor_change");
        }

        let mut broken_clients = Vec::new();
        let sent = self.send_retained_frame_to_client(client_id, frame, &mut broken_clients);
        for broken_client in broken_clients {
            self.remove_client_and_resize_if_needed(broken_client);
        }
        if sent {
            retained_success!("sent");
        }
        retained_fallback!("send_failed");
    }

    fn client_presentation_policy(
        &self,
        client_id: u64,
    ) -> Option<crate::app::state::ClientPresentationPolicy<'static>> {
        let client = self.clients.get(&client_id)?;
        Some(client.presentation_policy(&self.app.state))
    }

    fn retained_pty_update_allowed_by_app_state(&self) -> bool {
        self.app.state.server_mode() == app::Mode::Terminal
            && !self.app.state.tab_surface_replaced()
            && self.app.state.popup_pane.is_none()
            && self.app.state.selection.is_none()
            && self.app.state.copy_mode.is_none()
            && self.app.state.context_menu.is_none()
            && self.app.state.toast.is_none()
            && self.app.state.copy_feedback.is_none()
            && !self.app.full_redraw_pending
    }

    fn send_retained_frame_to_client(
        &mut self,
        client_id: u64,
        frame: FrameData,
        broken_clients: &mut Vec<u64>,
    ) -> bool {
        let Some(client) = self.clients.get_mut(&client_id) else {
            crate::render_prof::event("retained_send_fallback.client_missing");
            return false;
        };
        let Some(writer) = client.writer.as_ref().cloned() else {
            crate::render_prof::event("retained_send_fallback.writer_missing");
            return false;
        };
        let prepare_started = crate::render_prof::timer();
        let Some(prepared) = client.render_state.prepare_frame(frame) else {
            client.clear_deferred_render();
            crate::render_prof::event("retained_send.skip_identical");
            crate::render_prof::duration_since("retained_send.prepare_frame", prepare_started);
            return true;
        };
        crate::render_prof::duration_since("retained_send.prepare_frame", prepare_started);
        let serialize_started = crate::render_prof::timer();
        let serialized = match Self::frame_server_message(prepared.message()) {
            Ok(framed) => {
                crate::render_prof::duration_since("retained_send.serialize", serialize_started);
                framed
            }
            Err(protocol::FramingError::Oversized { claimed, max }) => {
                warn!(
                    client_id,
                    claimed, max, "skipping oversized retained frame for client"
                );
                crate::render_prof::event("retained_send_fallback.serialize_oversized");
                crate::render_prof::duration_since("retained_send.serialize", serialize_started);
                return false;
            }
            Err(err) => {
                warn!(client_id, err = %err, "failed to serialize retained frame for client");
                broken_clients.push(client_id);
                crate::render_prof::event("retained_send_fallback.serialize_error");
                crate::render_prof::duration_since("retained_send.serialize", serialize_started);
                return false;
            }
        };
        crate::render_prof::counter("retained_send.bytes", serialized.len() as u64);

        let send_started = crate::render_prof::timer();
        match writer.render.try_send(serialized) {
            Ok(_) => {
                client.clear_deferred_render();
                client.render_state.commit_sent_frame(prepared);
                crate::render_prof::event("retained_send.sent");
                crate::render_prof::duration_since("retained_send.try_send", send_started);
                true
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                client.defer_full_render();
                crate::render_prof::event("retained_send_fallback.queue_full");
                crate::render_prof::duration_since("retained_send.try_send", send_started);
                debug!(
                    client_id,
                    "render queue full, deferring latest retained frame"
                );
                false
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                debug!(client_id, "client writer channel closed, marking as broken");
                broken_clients.push(client_id);
                crate::render_prof::event("retained_send_fallback.writer_disconnected");
                crate::render_prof::duration_since("retained_send.try_send", send_started);
                false
            }
        }
    }

    fn render_and_stream(&mut self) {
        let full_started = crate::render_prof::timer();
        let render_targets = render_targets(&self.clients, self.foreground_client_id);

        if render_targets.is_empty() {
            let (cols, rows) = self.effective_size;
            let area = Rect::new(0, 0, cols, rows);
            let resize_panes = self.app.state.view.pane_infos.is_empty();
            let render_started = crate::render_prof::timer();
            let _ = crate::server::render_stream::render_virtual_with_runtime_registry(
                &mut self.app.state,
                &self.app.terminal_runtimes,
                area,
                resize_panes,
                crate::kitty_graphics::HostCellSize::default(),
            );
            crate::render_prof::duration_since("full_render.render_virtual", render_started);
            self.app.full_redraw_pending = false;
            self.app.agent_activity_refresh_deadline = None;
            crate::render_prof::duration_since("full_render.total", full_started);
            debug!(
                cols,
                rows, resize_panes, "rendered virtual frame with no attached clients"
            );
            return;
        }

        let mut broken_clients: Vec<u64> = Vec::new();
        let mut deferred_frame = false;
        let mut agent_activity_refresh_deadline = None;
        let mut changed_terminal_geometries = std::collections::HashSet::new();
        for (client_id, (cols, rows), cell_size, is_foreground, mode) in render_targets {
            let area = Rect::new(0, 0, cols, rows);
            let is_app_client = matches!(mode, ClientConnectionMode::App);
            let mut frame = match mode {
                ClientConnectionMode::App => {
                    let mut sidebar_presentation = self
                        .clients
                        .get_mut(&client_id)
                        .map(|client| std::mem::take(&mut client.sidebar_presentation))
                        .unwrap_or_default();
                    let mut loop_run_history_detail = self
                        .clients
                        .get_mut(&client_id)
                        .and_then(|client| client.loop_run_history_detail.take());
                    let mut aloop_run_detail = self
                        .clients
                        .get_mut(&client_id)
                        .and_then(|client| client.aloop_run_detail.take());
                    let mut symphony_detail = self
                        .clients
                        .get_mut(&client_id)
                        .and_then(|client| client.symphony_detail.take());
                    let mut work_view = self
                        .clients
                        .get_mut(&client_id)
                        .and_then(|client| client.work_view.take());
                    let mut usage_view = self
                        .clients
                        .get_mut(&client_id)
                        .and_then(|client| client.usage_view.take());
                    let preserved_scroll = (!is_foreground).then_some((
                        self.app.state.workspace_scroll,
                        self.app.state.agent_panel_scroll,
                        self.app.state.tab_scroll,
                        self.app.state.mobile_switcher_scroll,
                    ));
                    self.app
                        .state
                        .swap_sidebar_presentation(&mut sidebar_presentation);
                    let mut dock_presentation = self
                        .clients
                        .get_mut(&client_id)
                        .map(|client| std::mem::take(&mut client.dock_presentation))
                        .unwrap_or_default();
                    self.app
                        .state
                        .swap_dock_presentation(&mut dock_presentation);
                    let mut notepad_presentation = self
                        .clients
                        .get_mut(&client_id)
                        .map(|client| std::mem::take(&mut client.notepad_presentation))
                        .unwrap_or_default();
                    self.app
                        .state
                        .notepad
                        .swap_presentation(&mut notepad_presentation);
                    self.app.state.reconcile_dock_home_with_focused_pane();
                    self.app.state.reconcile_sidebar_presentation();
                    self.app
                        .state
                        .swap_loop_run_history_detail(&mut loop_run_history_detail);
                    self.app.state.swap_aloop_run_detail(&mut aloop_run_detail);
                    self.app.state.swap_symphony_detail(&mut symphony_detail);
                    self.app.state.swap_work_view(&mut work_view);
                    self.app.state.swap_usage_view(&mut usage_view);
                    let render_started = crate::render_prof::timer();
                    let render_cell_size = if cell_size.is_known() {
                        cell_size
                    } else {
                        crate::kitty_graphics::HostCellSize::default()
                    };
                    let (buffer, cursor) =
                        crate::server::render_stream::render_virtual_with_runtime_registry_reusing(
                            &mut self.app.state,
                            &self.app.terminal_runtimes,
                            area,
                            is_foreground,
                            render_cell_size,
                            &mut self
                                .clients
                                .get_mut(&client_id)
                                .expect("render target client remains connected")
                                .render_terminal,
                        );
                    self.app.record_pending_first_frame();
                    // The editor PTY is a shared runtime resource. Its size follows the
                    // foreground client's layout, while every app client still renders its
                    // own dock geometry above.
                    if is_foreground {
                        self.app.ensure_dock_editor();
                        self.app.resize_dock_editor();
                        self.app.ensure_scratchpad();
                        self.app.ensure_notepad();
                    }
                    if let Some(deadline) = self
                        .app
                        .state
                        .next_agent_activity_age_change(self.app.state.view_observed_at)
                    {
                        agent_activity_refresh_deadline = Some(
                            agent_activity_refresh_deadline
                                .map_or(deadline, |current: Instant| current.min(deadline)),
                        );
                    }
                    if let Some((workspace, agent_panel, tab, mobile_switcher)) = preserved_scroll {
                        self.app.state.workspace_scroll = workspace;
                        self.app.state.agent_panel_scroll = agent_panel;
                        self.app.state.tab_scroll = tab;
                        self.app.state.mobile_switcher_scroll = mobile_switcher;
                    }
                    crate::render_prof::duration_since(
                        "full_render.render_virtual",
                        render_started,
                    );
                    let hyperlinks_started = crate::render_prof::timer();
                    let hyperlinks = crate::server::render_stream::visible_hyperlinks(
                        &self.app.state,
                        &self.app.terminal_runtimes,
                    );
                    let hyperlink_spans =
                        crate::server::render_stream::visible_hyperlink_spans(&self.app.state);
                    crate::render_prof::duration_since(
                        "full_render.visible_hyperlinks",
                        hyperlinks_started,
                    );
                    let frame_started = crate::render_prof::timer();
                    let frame = FrameData::from_ratatui_buffer_with_hyperlinks_and_spans(
                        &buffer,
                        cursor,
                        &hyperlinks,
                        &hyperlink_spans,
                    );
                    let retained_pane_cursor =
                        !crate::server::render_stream::dock_editor_is_focused(&self.app.state);
                    let animation_rect = self.app.state.view.hyperspace_rect;
                    crate::render_prof::duration_since("full_render.frame_build", frame_started);
                    self.app
                        .state
                        .swap_sidebar_presentation(&mut sidebar_presentation);
                    self.app
                        .state
                        .swap_dock_presentation(&mut dock_presentation);
                    self.app
                        .state
                        .notepad
                        .swap_presentation(&mut notepad_presentation);
                    self.app
                        .state
                        .swap_loop_run_history_detail(&mut loop_run_history_detail);
                    self.app.state.swap_aloop_run_detail(&mut aloop_run_detail);
                    self.app.state.swap_symphony_detail(&mut symphony_detail);
                    self.app.state.swap_work_view(&mut work_view);
                    self.app.state.swap_usage_view(&mut usage_view);
                    changed_terminal_geometries
                        .extend(self.update_client_terminal_geometries(client_id));
                    if let Some(client) = self.clients.get_mut(&client_id) {
                        std::sync::Arc::make_mut(&mut client.retained_pane_infos)
                            .clone_from(&self.app.state.view.pane_infos);
                        client.retained_pane_cursor = retained_pane_cursor;
                        client.animation_rect = animation_rect;
                        client.sidebar_presentation = sidebar_presentation;
                        client.dock_presentation = dock_presentation;
                        client.notepad_presentation = notepad_presentation;
                        client.loop_run_history_detail = loop_run_history_detail;
                        client.aloop_run_detail = aloop_run_detail;
                        client.symphony_detail = symphony_detail;
                        client.work_view = work_view;
                        client.usage_view = usage_view;
                    }
                    frame
                }
                ClientConnectionMode::TerminalAttach { terminal_id, .. }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    let runtime = self
                        .app
                        .state
                        .terminals
                        .keys()
                        .find(|id| id.as_str() == terminal_id)
                        .and_then(|id| self.app.terminal_runtimes.get(id));
                    let Some(runtime) = runtime else {
                        self.send_to_client(
                            client_id,
                            ServerMessage::ServerShutdown {
                                reason: Some(format!(
                                    "terminal attach ended: terminal {terminal_id} not found"
                                )),
                            },
                        );
                        broken_clients.push(client_id);
                        continue;
                    };
                    let render_started = crate::render_prof::timer();
                    let (buffer, cursor) =
                        crate::server::render_stream::render_terminal_virtual_reusing(
                            runtime,
                            area,
                            &mut self
                                .clients
                                .get_mut(&client_id)
                                .expect("render target client remains connected")
                                .render_terminal,
                        );
                    crate::render_prof::duration_since(
                        "full_render.render_terminal_virtual",
                        render_started,
                    );
                    let hyperlinks_started = crate::render_prof::timer();
                    let hyperlinks = runtime.visible_hyperlinks(area);
                    crate::render_prof::duration_since(
                        "full_render.visible_hyperlinks",
                        hyperlinks_started,
                    );
                    let frame_started = crate::render_prof::timer();
                    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    );
                    crate::render_prof::duration_since("full_render.frame_build", frame_started);
                    frame
                }
            };

            let pomodoro_presentation = is_app_client.then(|| {
                crate::ui::pomodoro::input_presentation_at(
                    &self.app.state,
                    area,
                    self.app.state.view_observed_at,
                )
            });
            let client_presentation_policy = is_app_client
                .then(|| {
                    self.clients.get(&client_id).map(|client| {
                        client.presentation_policy_with_pomodoro(
                            &self.app.state,
                            pomodoro_presentation
                                .is_some_and(crate::ui::pomodoro::InputPresentation::owns_input),
                        )
                    })
                })
                .flatten();
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            #[cfg(unix)]
            let reset_graphics_cache = client
                .graphics_surface_reset_pending
                .then(crate::kitty_graphics::HostGraphicsCache::default);
            #[cfg(unix)]
            let mut direct_transfer = if client.direct_graphics
                && client.terminal_file_graphics
                && client.direct_terminal_graphics.is_none()
                && is_app_client
                && self.app.state.kitty_graphics_enabled
            {
                match crate::kitty_graphics::prepare_direct_terminal_transfer(
                    &self.app.pane_graphics_files,
                    &self.app.state,
                    &self.app.pane_graphics,
                    &self.app.terminal_runtimes,
                    client_presentation_policy
                        .as_ref()
                        .expect("app render target has presentation policy")
                        .clone(),
                    cell_size,
                    reset_graphics_cache
                        .as_ref()
                        .unwrap_or(&client.graphics_cache),
                ) {
                    Ok(transfer) => transfer,
                    Err(err) => {
                        warn!(client_id, %err, "failed to stage direct terminal graphics");
                        client.terminal_file_graphics = false;
                        crate::kitty_graphics::set_direct_host_graphics(false);
                        None
                    }
                }
            } else {
                None
            };
            #[cfg(unix)]
            let direct_serialized = direct_transfer
                .as_ref()
                .map(crate::kitty_graphics::DirectTerminalTransfer::message)
                .map(|message| {
                    Self::frame_server_message_with_max(&message, MAX_GRAPHICS_FRAME_SIZE)
                })
                .transpose();
            #[cfg(unix)]
            let direct_serialized = match direct_serialized {
                Ok(serialized) => serialized,
                Err(err) => {
                    warn!(client_id, %err, "failed to serialize direct terminal graphics");
                    direct_transfer = None;
                    None
                }
            };
            #[cfg(unix)]
            let direct_transfer_pending = client.direct_terminal_graphics.is_some();
            #[cfg(unix)]
            let direct_transfer_ready = direct_transfer.is_some();
            #[cfg(not(unix))]
            let direct_transfer_pending = false;
            #[cfg(not(unix))]
            let direct_transfer_ready = false;
            let mut next_graphics_cache = client.graphics_cache.clone();
            let mut reset_graphics = Vec::new();
            let mut encoded = if is_app_client
                && self.app.state.kitty_graphics_enabled
                && cell_size.is_known()
            {
                if client.graphics_surface_reset_pending {
                    if self.app.pane_graphics.slots.is_empty() {
                        reset_graphics = next_graphics_cache.clear_bytes();
                    } else {
                        next_graphics_cache = crate::kitty_graphics::HostGraphicsCache::default();
                    }
                }
                let graphics_started = crate::render_prof::timer();
                let encoded = if direct_transfer_ready || direct_transfer_pending {
                    crate::kitty_graphics::EncodedGraphics {
                        bytes: Vec::new(),
                        incomplete: false,
                    }
                } else {
                    crate::kitty_graphics::encode_local_pane_graphics_for_client(
                        &self.app.state,
                        &self.app.pane_graphics,
                        &self.app.terminal_runtimes,
                        client_presentation_policy
                            .expect("app render target has presentation policy"),
                        cell_size,
                        Some(crate::kitty_graphics::HEADLESS_GRAPHICS_TRANSACTION_BUDGET),
                        &mut next_graphics_cache,
                    )
                };
                crate::render_prof::duration_since("full_render.graphics_encode", graphics_started);
                encoded
            } else if self.app.pane_graphics.slots.is_empty() {
                crate::kitty_graphics::EncodedGraphics {
                    bytes: next_graphics_cache.clear_bytes(),
                    incomplete: false,
                }
            } else {
                next_graphics_cache.clear_next()
            };
            if !reset_graphics.is_empty() {
                reset_graphics.extend(encoded.bytes);
                encoded.bytes = reset_graphics;
            }
            frame.graphics = encoded.bytes;

            let Some(writer) = client.writer.as_ref().cloned() else {
                crate::render_prof::event("full_render.writer_missing");
                continue;
            };
            #[cfg(unix)]
            if direct_transfer.is_some() {
                client.request_repaint();
            }
            let mut commit_graphics_cache = true;
            if frame.graphics.len() > MAX_GRAPHICS_FRAME_SIZE {
                warn!(
                    client_id,
                    graphics_bytes = frame.graphics.len(),
                    max = MAX_GRAPHICS_FRAME_SIZE,
                    "dropping oversized graphics payload for client frame"
                );
                frame.graphics.clear();
                commit_graphics_cache = false;
                encoded.incomplete = false;
            }
            let has_graphics = !frame.graphics.is_empty();
            let Some(mut prepared) = client.render_state.prepare_frame(frame) else {
                if commit_graphics_cache {
                    client.graphics_cache = next_graphics_cache;
                    client.graphics_surface_reset_pending = false;
                }
                if encoded.incomplete {
                    client.defer_full_render();
                    deferred_frame = true;
                } else {
                    client.clear_deferred_render();
                }
                crate::render_prof::event("full_render.skip_identical");
                continue;
            };
            let max = if has_graphics {
                MAX_GRAPHICS_FRAME_SIZE
            } else {
                crate::protocol::MAX_FRAME_SIZE
            };
            let serialized = match Self::frame_server_message_with_max(prepared.message(), max) {
                Ok(frame) => frame,
                Err(protocol::FramingError::Oversized { claimed, max }) if has_graphics => {
                    warn!(
                        client_id,
                        claimed, max, "dropping graphics from oversized frame for client"
                    );
                    let Some(mut text_only_frame) = prepared.into_frame() else {
                        crate::render_prof::event("full_render.serialize_error");
                        continue;
                    };
                    text_only_frame.graphics.clear();
                    let Some(text_only_prepared) =
                        client.render_state.prepare_frame(text_only_frame)
                    else {
                        client.clear_deferred_render();
                        crate::render_prof::event("full_render.skip_identical_text_only");
                        continue;
                    };
                    let framed = match Self::frame_server_message(text_only_prepared.message()) {
                        Ok(framed) => framed,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to serialize text-only frame for client");
                            broken_clients.push(client_id);
                            crate::render_prof::event("full_render.serialize_error");
                            continue;
                        }
                    };
                    prepared = text_only_prepared;
                    commit_graphics_cache = false;
                    encoded.incomplete = false;
                    framed
                }
                Err(protocol::FramingError::Oversized { claimed, max }) => {
                    warn!(
                        client_id,
                        claimed, max, "skipping oversized frame for client"
                    );
                    crate::render_prof::event("full_render.serialize_oversized");
                    continue;
                }
                Err(err) => {
                    warn!(client_id, err = %err, "failed to serialize frame");
                    broken_clients.push(client_id);
                    crate::render_prof::event("full_render.serialize_error");
                    continue;
                }
            };
            match writer.render.try_send(serialized) {
                Ok(render_sequence) => {
                    if commit_graphics_cache {
                        client.graphics_cache = next_graphics_cache;
                        client.graphics_surface_reset_pending = false;
                    }
                    client.render_state.commit_sent_frame(prepared);
                    #[cfg(unix)]
                    if let (Some(transfer), Some(serialized)) =
                        (direct_transfer.take(), direct_serialized)
                    {
                        match writer.render.send_ordered(serialized) {
                            Ok(()) => client.direct_terminal_graphics = Some(transfer),
                            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                                client.defer_full_render();
                                deferred_frame = true;
                            }
                            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                                broken_clients.push(client_id);
                            }
                        }
                    }
                    if let Some(presentation) = pomodoro_presentation {
                        client.queue_pomodoro_presentation(render_sequence, presentation);
                    }
                    if encoded.incomplete {
                        client.defer_full_render();
                        deferred_frame = true;
                    } else {
                        client.clear_deferred_render();
                    }
                    crate::render_prof::event("full_render.sent");
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                    deferred_frame = true;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    broken_clients.push(client_id);
                }
            }
        }

        if !broken_clients.is_empty() {
            for client_id in broken_clients {
                self.remove_client_and_resize_if_needed(client_id);
            }
        }
        self.reconcile_terminal_geometries(changed_terminal_geometries);

        self.app.agent_activity_refresh_deadline = agent_activity_refresh_deadline;
        let (cols, rows) = self.effective_size;
        if !deferred_frame {
            self.app.full_redraw_pending = false;
        }
        crate::render_prof::duration_since("full_render.total", full_started);
        debug!(cols, rows, foreground_client_id = ?self.foreground_client_id, "rendered virtual frame(s)");
    }

    /// Handle scheduled tasks for the headless server.
    ///
    /// Similar to `App::handle_scheduled_tasks` but without resize polling
    /// (the server doesn't have a terminal to resize).
    #[cfg(test)]
    fn handle_scheduled_tasks_headless(&mut self, now: Instant, geometry_dirty: bool) -> bool {
        self.handle_scheduled_tasks_headless_with_render_kind(now, geometry_dirty)
            .changed
    }

    fn handle_scheduled_tasks_headless_with_render_kind(
        &mut self,
        now: Instant,
        geometry_dirty: bool,
    ) -> ScheduledTaskRender {
        // Nothing renders the status row without an attached app client, so a
        // detached server never samples native metrics.
        let has_app_client = self.has_app_client();
        // A client resize updates its announced geometry before that geometry
        // has produced a frame. Wait for the pending render to finish so a
        // narrow-to-wide resize cannot start collection for an unseen row.
        self.app.status_metrics_visible = !geometry_dirty && self.has_renderable_status_target();
        let mut other_changed = if has_app_client {
            self.app.take_due_agent_activity_refresh(now)
        } else {
            // Activity age is client-only presentation state. Drop a deadline
            // left by the final rendered frame when the last app client
            // detaches instead of turning an unrelated server wake into a
            // render request.
            self.app.agent_activity_refresh_deadline = None;
            false
        };
        let mut sidebar_animation_changed = false;
        for client in self.clients.values_mut() {
            if client.dock_presentation.reveal_hover_tooltip_at(now) {
                client.request_repaint();
                other_changed = true;
            }
        }
        other_changed |= self.app.handle_loop_receipt_fallback(now);
        other_changed |= self.app.tick_notepad(now);
        if has_app_client {
            other_changed |= self.app.schedule_goals_refresh(now);
            let host_focused = self.app_clients_host_focused();
            other_changed |= self.app.tick_pomodoro(now, host_focused);
            other_changed |= self.pomodoro_animation_due(now);
            // The sidebar only exists in front of an attached client, and this
            // loop - not `App::handle_scheduled_tasks` - is the one every
            // server-backed session actually runs.
            sidebar_animation_changed = self.app.tick_sidebar_animation(now);
        }
        if self.app.status_metrics_visible {
            other_changed |= self.app.schedule_status_metrics(now);
            self.app.schedule_status_side_signals(now);
        } else {
            other_changed |= self.app.discard_stale_status_metrics(now);
        }
        if has_app_client
            && (self.app.state.notepad.enabled
                || self.app.state.usage_view.is_some()
                || self.app.state.home.is_some())
        {
            self.app.schedule_provider_usage(now);
        }

        // No resize polling needed — server has no terminal.
        // Client resize messages drive size changes instead.

        if self
            .app
            .config_diagnostic_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.config_diagnostic_deadline = None;
            self.app.state.config_diagnostic = None;
            other_changed = true;
        }

        if self
            .app
            .toast_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.toast_deadline = None;
            self.app.state.toast = None;
            other_changed = true;
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
                other_changed = true;
            }
        }

        if self
            .app
            .state
            .next_agent_watchdog_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            for update in self.app.state.mark_due_agent_status_stale_at(now) {
                self.app.emit_pane_state_update(&update);
                other_changed = true;
            }
            // The mark is what re-enables the process probe under a hook.
            self.app.sync_detection_authority_mirrors();
        }

        if has_app_client && self.app.state.done_hide_transition_due(now) {
            other_changed = true;
        }
        if self
            .app
            .state
            .next_done_reap_deadline(now)
            .is_some_and(|deadline| now >= deadline)
        {
            other_changed |= self.app.reap_due_done_panes(now);
        }

        if self
            .app
            .state
            .next_full_lifecycle_hook_authority_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let (updates, due) = self
                .app
                .state
                .expire_due_full_lifecycle_hook_authority_at(now);
            self.app.sync_detection_authority_mirrors();
            for update in &updates {
                self.app.emit_pane_state_update(update);
            }
            for (ws_idx, pane_id) in due {
                self.app.emit_pane_updated(ws_idx, pane_id);
                other_changed = true;
            }
        }

        if self
            .app
            .copy_feedback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.copy_feedback_deadline = None;
            self.app.state.copy_feedback = None;
            other_changed = true;
        }

        if self
            .app
            .selection_autoscroll_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.tick_selection_autoscroll(now);
            other_changed = true;
        }

        other_changed |= self.app.clear_due_selection_highlight(now);
        other_changed |= self.app.process_git_action_panes(now);
        other_changed |= self.app.refresh_pane_settlement_at(now);

        if self.has_app_client() {
            // Work-context links matter only while a TUI is attached and viewing panes.
            self.app.start_git_work_context_refresh_if_due(now);
            self.app.dock_files_refresh_demand = self
                .foreground_client_id
                .and_then(|client_id| self.clients.get(&client_id))
                .is_some_and(|client| {
                    !client.dock_presentation.collapsed
                        && client.dock_presentation.tab == Some(crate::app::DockSurface::Files)
                });
            if self.app.dock_files_refresh_demand {
                self.app.start_dock_files_refresh();
            }
            self.app.start_git_status_refresh_if_due(now);
            other_changed |= self.start_foreground_dock_diff_refresh_if_needed();
        } else {
            self.app.dock_files_refresh_demand = false;
        }
        // Without a TUI, only idle agents need foreground-child promotion. Keeping the
        // target set narrow avoids recurring process-tree scans for ordinary API panes.
        if has_app_client {
            self.app.start_foreground_process_refresh_if_due(now);
        } else {
            self.app
                .start_headless_foreground_process_refresh_if_due(now);
        }
        self.app.start_claude_subagent_refresh_if_due(now);
        self.app.observe_status_changes(now);
        // The work index is a server-owned runtime fact, so it refreshes with or
        // without an attached TUI. Omitting it here left every server-backed
        // session with a permanently empty index while the interactive loop
        // refreshed fine, which is the #119 defect class.
        if self.work_index_refresh_is_useful(now) {
            self.app.start_work_index_refresh_if_due(now);
        }
        let detail_request = self
            .foreground_client_id
            .and_then(|client_id| self.clients.get(&client_id))
            .and_then(work_item_detail_request);
        let (section, selection, detail_visible) =
            detail_request.unwrap_or((crate::app::state::DockHomeSection::Prs, None, false));
        self.app
            .start_work_item_detail_refresh_if_due(now, section, selection, detail_visible);

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
            other_changed = true;
        }

        if geometry_dirty {
            self.app.pending_agent_resume_deadline = None;
        } else {
            self.app.sync_pending_agent_resume_deadline(now);
            other_changed |= self
                .app
                .start_pending_agent_resumes(self.app.pending_agent_resume_due(now));
        }
        // The headless server owns its own scheduler, so anything the TUI loop
        // ticks has to be ticked here too or it only runs for TUI-owned
        // runtimes. Resumes above, and the nudge that follows them.
        other_changed |= self.app.tick_resume_nudges(now);
        other_changed |= self.app.tick_auto_nudges(now);
        ScheduledTaskRender {
            changed: other_changed || sidebar_animation_changed,
            sidebar_animation_only: sidebar_animation_changed && !other_changed,
        }
    }

    fn start_foreground_dock_diff_refresh_if_needed(&mut self) -> bool {
        let Some(client_id) = self.foreground_client_id else {
            return false;
        };
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let mut presentation = std::mem::take(&mut client.dock_presentation);
        let before = presentation.clone();
        self.app.state.swap_dock_presentation(&mut presentation);
        self.app.start_dock_diff_refresh_if_needed();
        self.app.state.swap_dock_presentation(&mut presentation);
        let changed = presentation != before;
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.dock_presentation = presentation;
        }
        changed
    }

    /// Initiates graceful shutdown.
    fn initiate_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        info!("server shutdown initiated");
        self.shutting_down = true;
        self.teardown_remote_focus_proxies();

        // Clear client-local host graphics, then send ServerShutdown to all connected clients.
        self.send_all_clients_graphics_cleanup();
        let shutdown_msg = ServerMessage::ServerShutdown {
            reason: Some("server is shutting down".to_owned()),
        };
        self.send_to_all_clients(shutdown_msg);

        // Give client writer threads a moment to flush the shutdown message.
        // A short sleep ensures the message is written to the socket before
        // we close the connections.
        std::thread::sleep(Duration::from_millis(50));

        // Signal the main loop to exit.
        self.should_quit.store(true, Ordering::Release);
        self.app.state.should_quit = true;
    }

    /// Completes the shutdown sequence: send ServerShutdown to clients,
    /// close client connections, remove socket files, and clean up.
    async fn complete_shutdown(&mut self) -> io::Result<()> {
        info!("completing server shutdown");
        self.teardown_remote_focus_proxies();
        self.reject_late_client_connections().await;

        // Send ServerShutdown to all remaining clients.
        if !self.clients.is_empty() {
            self.send_all_clients_graphics_cleanup();
            let shutdown_msg = ServerMessage::ServerShutdown {
                reason: Some("server is shutting down".to_owned()),
            };
            self.send_to_all_clients(shutdown_msg);

            // Give writer threads a moment to flush before closing.
            std::thread::sleep(Duration::from_millis(50));
        }

        // Reject only the requests already queued when shutdown reached cleanup.
        self.reject_queued_api_requests_for_shutdown();

        // Close all client connections.
        let staged_files = self
            .clients
            .drain()
            .flat_map(|(_, client)| client.staged_clipboard_files)
            .collect::<Vec<_>>();
        crate::server::clipboard_image::remove_files(staged_files);

        // Remove socket files.
        self.cleanup_sockets()?;

        Ok(())
    }

    /// Removes socket files created by the server.
    fn cleanup_sockets(&self) -> io::Result<()> {
        if let Err(err) =
            remove_socket_file_if_owned(&self.client_socket_path, &self.client_socket_identity)
        {
            if err.kind() != io::ErrorKind::NotFound {
                warn!(
                    path = %self.client_socket_path.display(),
                    err = %err,
                    "failed to remove client socket on shutdown"
                );
            }
        }
        Ok(())
    }
}

// Pane applications render their own motion responses through PTY output. Only Herdr modes with
// hover selection mutate the current frame directly from a plain mouse-move event.
fn events_are_render_neutral_mouse_motion(
    events: &[crate::raw_input::RawInputEvent],
    mode: crate::app::Mode,
) -> bool {
    !events.is_empty()
        && !mode.mouse_motion_changes_view()
        && events.iter().all(|event| {
            matches!(
                event,
                crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::Moved,
                    ..
                })
            )
        })
}

fn events_for_app_routing(
    events: Vec<crate::raw_input::RawInputEvent>,
    mut source_is_foreground: bool,
    source_is_full_app: bool,
) -> Vec<crate::raw_input::RawInputEvent> {
    events
        .into_iter()
        .filter_map(|event| match event {
            crate::raw_input::RawInputEvent::OuterFocusGained
            | crate::raw_input::RawInputEvent::OuterFocusLost
                if !source_is_full_app =>
            {
                None
            }
            crate::raw_input::RawInputEvent::OuterFocusGained => {
                source_is_foreground = true;
                Some(event)
            }
            crate::raw_input::RawInputEvent::OuterFocusLost if !source_is_foreground => None,
            crate::raw_input::RawInputEvent::Key(_)
            | crate::raw_input::RawInputEvent::Text(_)
            | crate::raw_input::RawInputEvent::Mouse(_)
            | crate::raw_input::RawInputEvent::Paste(_) => {
                source_is_foreground = true;
                Some(event)
            }
            _ => Some(event),
        })
        .collect()
}

impl Drop for HeadlessServer {
    fn drop(&mut self) {
        self.teardown_remote_focus_proxies();
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
    let without_keybindings = diagnostics
        .iter()
        .filter(|diagnostic| !is_keybinding_config_diagnostic(diagnostic))
        .cloned()
        .collect::<Vec<_>>();
    (
        config::config_diagnostic_summary(diagnostics),
        config::config_diagnostic_summary(&without_keybindings),
    )
}

fn is_keybinding_config_diagnostic(diagnostic: &str) -> bool {
    diagnostic.contains("keybinding") || diagnostic.contains("keys.")
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the headless server. This is the entry point called from main.rs.
pub fn run_server() -> io::Result<()> {
    init_logging();
    crate::platform::raise_server_nofile_limit();

    let args: Vec<String> = std::env::args().collect();
    if args.get(2).map(String::as_str) == Some("--handoff-import") {
        let socket_path = args
            .get(3)
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing handoff socket"))?;
        let token = args
            .get(4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing handoff token"))?;
        return run_handoff_import_server(&socket_path, token);
    }

    let loaded_config = config::Config::load();
    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();
    let should_quit = Arc::new(AtomicBool::new(false));

    // Start the JSON API socket server.
    let _api_server = match api::start_server_with_stop_control(
        api_tx.clone(),
        event_hub.clone(),
        should_quit.clone(),
    ) {
        Ok(server) => server,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("error: herdr server is already running");
            eprintln!("api socket: {}", api::socket_path().display());
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    let no_session = false; // Server always does session persistence.

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let result = rt.block_on(async {
        // Create the App (with AppState, event channels, etc.).
        let mut app = app::App::new(
            &loaded_config.config,
            no_session,
            config::config_diagnostic_summary(&loaded_config.diagnostics),
            api_rx,
            event_hub,
        );
        seed_startup_workspace_if_empty(&mut app);
        app.state.open_home_on_launch(&loaded_config.config);

        // The server runs headless — disable local notification side effects.
        // Sound and terminal notifications are forwarded to connected clients
        // as ServerMessage::Notify instead of emitted by the server process.
        // The prefix input-source switch is likewise forwarded to the foreground
        // client (ServerMessage::PrefixInputSource), never applied in-process.
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;
        app.local_input_source_switch = false;

        // Create the headless server.
        let mut server = match HeadlessServer::new(
            app,
            &loaded_config.diagnostics,
            Some(api_tx.clone()),
            Some(_api_server),
            should_quit,
        ) {
            Ok(server) => server,
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                eprintln!("error: herdr server is already running");
                eprintln!("client socket: {}", client_socket_path().display());
                std::process::exit(1);
            }
            Err(err) => return Err(err),
        };

        info!(
            api_socket = %api::socket_path().display(),
            client_socket = %client_socket_path().display(),
            "herdr server started"
        );
        print_ready_message(&api::socket_path(), &client_socket_path());
        server.app.run_plugin_startup_hooks();

        server.run().await
    });

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("server");
    result
}

fn seed_startup_workspace_if_empty(app: &mut app::App) {
    let Some(cwd) = take_startup_cwd() else {
        return;
    };

    if !app.state.workspaces.is_empty() {
        info!(
            cwd = %cwd.display(),
            "restored session already has workspaces; ignoring startup cwd"
        );
        return;
    }

    match app.create_workspace_with_options(cwd.clone(), true) {
        Ok(_) => {
            info!(cwd = %cwd.display(), "created startup workspace");
        }
        Err(err) => {
            warn!(cwd = %cwd.display(), err = %err, "failed to create startup workspace");
            app.state.set_server_mode(app::Mode::Navigate);
        }
    }
}

fn take_startup_cwd() -> Option<PathBuf> {
    let cwd = std::env::var_os(crate::server::autodetect::STARTUP_CWD_ENV_VAR)?;
    std::env::remove_var(crate::server::autodetect::STARTUP_CWD_ENV_VAR);
    (!cwd.is_empty()).then(|| PathBuf::from(cwd))
}

#[cfg(unix)]
fn run_handoff_import_server(socket_path: &Path, token: &str) -> io::Result<()> {
    let loaded_config = config::Config::load();
    let mut received = crate::server::handoff::receive(socket_path, token)?;
    crate::server::handoff::log_import_result(received.manifest.panes.len());

    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();
    let should_quit = Arc::new(AtomicBool::new(false));

    let dock_editors = std::mem::take(&mut received.manifest.dock_editors);
    let mut imports = HashMap::new();
    for (pane, fd) in received.manifest.panes.drain(..).zip(received.fds) {
        let pane_id = pane.pane_id;
        imports.insert(
            pane_id,
            crate::handoff_runtime::ImportedHandoffRuntime {
                master_fd: fd,
                state: pane,
            },
        );
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let result = rt.block_on(async {
        let mut app = app::App::new_from_handoff(
            &loaded_config.config,
            config::config_diagnostic_summary(&loaded_config.diagnostics),
            api_rx,
            event_hub.clone(),
            &received.manifest.snapshot,
            &mut imports,
            &dock_editors,
        )?;
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;
        app.local_input_source_switch = false;
        app.state.open_home_on_launch(&loaded_config.config);
        crate::server::handoff::report_restored(&mut received.stream)?;
        if std::env::var("HERDR_TEST_HANDOFF_IMPORT_FAIL").as_deref() == Ok("after_restored") {
            return Err(io::Error::other(
                "test handoff import failure after restored",
            ));
        }
        wait_for_old_public_sockets_to_close(Duration::from_secs(5))?;

        let api_server = api::start_server_with_stop_control(
            api_tx.clone(),
            event_hub.clone(),
            should_quit.clone(),
        )?;
        let mut server = HeadlessServer::new(
            app,
            &loaded_config.diagnostics,
            Some(api_tx.clone()),
            Some(api_server),
            should_quit,
        )?;
        // Carried across before any client attaches, so the first title sent is
        // the override rather than the configured one it replaced.
        server.api_window_title = received.manifest.api_window_title.take();
        crate::server::handoff::report_ready(&mut received.stream)?;
        crate::server::handoff::wait_committed(&mut received.stream)?;
        server.app.assume_handoff_ownership();
        server.app.unpause_handoff_readers();
        server.pending_handoff_repaint_nudge = true;
        if let Err(err) = crate::server::handoff::report_owned(&mut received.stream) {
            warn!(err = %err, "failed to report handoff ownership; continuing as owner");
        }
        info!("handoff import server started");
        print_ready_message(&api::socket_path(), &client_socket_path());
        server.app.run_plugin_startup_hooks();
        server.run().await
    });

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("server");
    result
}

#[cfg(unix)]
fn wait_for_old_public_sockets_to_close(timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let api_socket = api::socket_path();
    let client_socket = client_socket_path();
    while Instant::now() < deadline {
        let api_open = api_socket.exists() && crate::ipc::connect_local_stream(&api_socket).is_ok();
        let client_open =
            client_socket.exists() && crate::ipc::connect_local_stream(&client_socket).is_ok();
        if !api_open && !client_open {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "old server sockets did not close before handoff import bind",
    ))
}

#[cfg(not(unix))]
fn run_handoff_import_server(_socket_path: &Path, _token: &str) -> io::Result<()> {
    Err(io::Error::other("live handoff is only supported on Unix"))
}

fn print_ready_message(api_socket: &Path, client_socket: &Path) {
    eprintln!("herdr server running; you can use any herdr CLI command in another terminal.");
    eprintln!("api socket: {}", api_socket.display());
    eprintln!("client socket: {}", client_socket.display());
    eprintln!(
        "logs: {}",
        crate::session::data_dir()
            .join("herdr-server.log")
            .display()
    );
    eprintln!("did you mean to open the Herdr TUI? run `herdr`; you do not need `herdr server`.");
}

/// Initialize logging for the server process.
fn init_logging() {
    crate::logging::init_file_logging("herdr-server.log");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    #[cfg(unix)]
    use crate::app::remote_focus::RemoteFocusTransport;
    use crate::app::AppState;
    #[cfg(unix)]
    use crate::protocol::PROTOCOL_VERSION;
    use crate::protocol::{CellData, CursorState};
    use unicode_width::UnicodeWidthStr;

    #[path = "../idle_render_tests.rs"]
    mod idle_render_tests;
    #[path = "pane_graphics.rs"]
    mod pane_graphics_tests;

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("captured log lock").write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedLogWriter(self.0.clone())
        }
    }

    #[test]
    fn failed_live_handoff_warns_with_the_error_reason() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(logs.clone())
            .finish();

        let (response, succeeded) = tracing::subscriber::with_default(subscriber, || {
            live_handoff_api_response(
                "test:handoff".into(),
                Err(io::Error::other("replacement refused import")),
            )
        });

        assert!(!succeeded);
        assert!(response.contains("replacement refused import"));
        let output = String::from_utf8(logs.0.lock().expect("captured log lock").clone())
            .expect("logs are utf-8");
        assert!(output.contains("WARN"));
        assert!(output.contains("live handoff failed"));
        assert!(output.contains("replacement refused import"));
    }

    #[test]
    fn retained_render_plan_covers_each_render_path() {
        assert_eq!(
            retained_render_plan(RetainedRenderInput {
                needs_full_render: true,
                needs_graphics_render: true,
                pty: PtyRenderState::Hidden,
            }),
            RetainedRenderPlan::Full
        );
        assert_eq!(
            retained_render_plan(RetainedRenderInput {
                needs_full_render: false,
                needs_graphics_render: true,
                pty: PtyRenderState::Hidden,
            }),
            RetainedRenderPlan::Graphics
        );
        assert_eq!(
            retained_render_plan(RetainedRenderInput {
                needs_full_render: false,
                needs_graphics_render: false,
                pty: PtyRenderState::Visible,
            }),
            RetainedRenderPlan::Pty
        );
        assert_eq!(
            retained_render_plan(RetainedRenderInput {
                needs_full_render: false,
                needs_graphics_render: false,
                pty: PtyRenderState::Hidden,
            }),
            RetainedRenderPlan::HiddenPty
        );
    }

    fn test_headless_server() -> HeadlessServer {
        test_headless_server_with_event_hub(api::EventHub::default())
    }

    fn test_headless_server_with_event_hub(event_hub: api::EventHub) -> HeadlessServer {
        let mut config = crate::config::Config::default();
        config.work_index.enabled = false;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(&config, true, None, api_rx, event_hub);
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;
        app.local_input_source_switch = false;

        // A wall-clock suffix is not unique: two tests entering this helper in
        // the same clock tick derive the same directory and race to bind the
        // same socket, which surfaces as AddrInUse. A counter cannot collide.
        let dir = std::env::temp_dir().join(format!("hh-{}", crate::config::test_unique_suffix()));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("client.sock");
        let _ = fs::remove_file(&socket_path);
        let listener = bind_local_listener(&socket_path).expect("bind test listener");
        let client_socket_identity =
            socket_file_identity(&socket_path).expect("test listener socket identity");
        #[cfg(unix)]
        listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .expect("set listener nonblocking");
        let (server_event_tx, server_event_rx) = mpsc::channel(64);
        let should_quit = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        spawn_windows_client_accept_thread(listener, should_quit.clone(), server_event_tx.clone());
        let server_keybindings = app_keybindings(&app);
        let headless_size = app.state.headless_size;

        HeadlessServer {
            app,
            #[cfg(unix)]
            api_tx: None,
            api_server: None,
            #[cfg(unix)]
            client_listener: listener,
            client_socket_path: socket_path,
            client_socket_identity,
            clients: HashMap::new(),
            last_app_client_seen: Instant::now(),
            #[cfg(unix)]
            next_client_id: 1,
            foreground_client_id: None,
            sent_window_title: None,
            api_window_title: None,
            server_keybindings,
            server_config_diagnostic: None,
            server_config_diagnostic_without_keybindings: None,
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
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_submit_and_completion_stay_with_owning_client() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("source")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        let source_workspace_id = server.app.state.workspaces[0].id.clone();
        let source_path = std::env::temp_dir().join(format!(
            "hh-worktree-owner-source-{}",
            crate::config::test_unique_suffix()
        ));
        let worktree_root = std::env::temp_dir().join(format!(
            "hh-worktree-owner-root-{}",
            crate::config::test_unique_suffix()
        ));
        server.app.state.worktree_directory = worktree_root.clone();
        server.app.state.workspaces[0].worktree_space =
            Some(crate::workspace::WorktreeSpaceMembership {
                key: "repo-key".into(),
                label: "herdr".into(),
                repo_root: source_path.clone(),
                checkout_path: source_path.clone(),
                is_linked_worktree: false,
            });

        for client_id in [1, 2] {
            let (writer, _control_rx, _render_rx) = test_client_writer();
            server.clients.insert(
                client_id,
                ClientConnection::new(
                    (100, 30),
                    crate::kitty_graphics::HostCellSize::default(),
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    client_id,
                    RenderEncoding::SemanticFrame,
                    Some(writer),
                ),
            );
        }
        server.app.state.set_server_mode(crate::app::Mode::Settings);
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let client_one_path =
            crate::worktree::default_checkout_path(&worktree_root, "herdr", "owner/client-one");
        let client_two_path =
            crate::worktree::default_checkout_path(&worktree_root, "herdr", "owner/client-two");
        for (client_id, branch, checkout_path) in [
            (1, "owner/client-one", client_one_path.clone()),
            (2, "owner/client-two", client_two_path.clone()),
        ] {
            let presentation = &mut server
                .clients
                .get_mut(&client_id)
                .expect("client")
                .sidebar_presentation;
            presentation.overlay.kind = crate::app::state::ClientOverlay::NewLinkedWorktree;
            presentation.overlay.name_input = branch.into();
            presentation.overlay.worktree_create = Some(crate::app::state::WorktreeCreateState {
                source_workspace_id: source_workspace_id.clone(),
                source_checkout_path: source_path.clone(),
                source_existing_membership: server.app.state.workspaces[0].worktree_space.clone(),
                source_repo_root: source_path.clone(),
                repo_key: "repo-key".into(),
                repo_name: "herdr".into(),
                branch: branch.into(),
                checkout_path,
                error: None,
                creating: false,
            });
        }

        let enter = crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Enter,
            KeyModifiers::empty(),
        ));
        assert!(server.handle_client_input_events(1, vec![enter]));
        assert!(server.clients[&1]
            .sidebar_presentation
            .overlay
            .worktree_create
            .as_ref()
            .is_some_and(|draft| draft.creating));
        assert!(server.clients[&2]
            .sidebar_presentation
            .overlay
            .worktree_create
            .as_ref()
            .is_some_and(|draft| !draft.creating && draft.error.is_none()));

        let completion = tokio::time::timeout(Duration::from_secs(5), server.app.event_rx.recv())
            .await
            .expect("worktree completion timeout")
            .expect("worktree completion event");
        let AppEvent::WorktreeAddFinished(result) = &completion else {
            panic!("unexpected completion: {completion:?}");
        };
        assert_eq!(
            result
                .api_request
                .as_ref()
                .and_then(|request| request.client_id),
            Some(1)
        );
        assert!(server.handle_internal_event_with_forwarding(completion));

        let client_one = server.clients[&1]
            .sidebar_presentation
            .overlay
            .worktree_create
            .as_ref()
            .expect("client one draft");
        assert!(!client_one.creating);
        assert!(client_one.error.is_some());
        let client_two = server.clients[&2]
            .sidebar_presentation
            .overlay
            .worktree_create
            .as_ref()
            .expect("client two draft");
        assert!(!client_two.creating);
        assert!(client_two.error.is_none());
        assert_eq!(server.app.state.server_mode(), crate::app::Mode::Settings);

        shutdown_test_runtimes(&mut server);
        let _ = std::fs::remove_dir_all(worktree_root);
    }

    struct RecordingRemoteFocusTransport {
        detached: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl crate::app::remote_focus::RemoteFocusTransport for RecordingRemoteFocusTransport {
        fn start(
            &mut self,
            _operation_id: &str,
            _agent_ref: &api::schema::AgentRef,
            _proxy_pane_id: &str,
            _channels: crate::pane::RemoteProxyChannels,
            _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        ) -> Result<(), api::schema::ErrorBody> {
            Ok(())
        }

        fn detach(&mut self, operation_id: &str) {
            self.detached
                .lock()
                .expect("detach recording lock")
                .push(operation_id.to_owned());
        }
    }

    fn server_with_remote_focus_proxy() -> (
        HeadlessServer,
        String,
        crate::layout::PaneId,
        crate::terminal::TerminalId,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let mut server = test_headless_server();
        server.app.state.workspaces =
            vec![crate::workspace::Workspace::test_new("proxy-lifecycle")];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let detached = Arc::new(std::sync::Mutex::new(Vec::new()));
        server.app.remote_focus_transport = Box::new(RecordingRemoteFocusTransport {
            detached: Arc::clone(&detached),
        });
        let started = server
            .app
            .start_remote_focus_operation(
                api::schema::AgentRef::new("buildbox", "proxy-lifecycle:p1")
                    .expect("proxy agent ref"),
            )
            .expect("proxy operation starts");
        let (pane_id, terminal_id) = server
            .app
            .remote_focus_operations
            .proxy_location(&started.operation_id)
            .expect("proxy location");
        (server, started.operation_id, pane_id, terminal_id, detached)
    }

    fn assert_remote_focus_proxy_torn_down(
        server: &mut HeadlessServer,
        operation_id: &str,
        pane_id: crate::layout::PaneId,
        terminal_id: &crate::terminal::TerminalId,
        detached: &Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        assert_remote_focus_proxy_torn_down_with_state(
            server,
            operation_id,
            pane_id,
            terminal_id,
            detached,
            api::schema::RemoteFocusState::Closed,
        );
    }

    fn assert_remote_focus_proxy_torn_down_with_state(
        server: &mut HeadlessServer,
        operation_id: &str,
        pane_id: crate::layout::PaneId,
        terminal_id: &crate::terminal::TerminalId,
        detached: &Arc<std::sync::Mutex<Vec<String>>>,
        expected_state: api::schema::RemoteFocusState,
    ) {
        assert!(!server
            .app
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.pane_state(pane_id).is_some()));
        assert!(!server.app.state.terminals.contains_key(terminal_id));
        assert!(server.app.terminal_runtimes.get(terminal_id).is_none());
        assert_eq!(
            detached.lock().expect("detach recording lock").as_slice(),
            [operation_id]
        );
        assert_eq!(
            server
                .app
                .remote_focus_status(operation_id)
                .expect("proxy operation status")
                .state,
            expected_state
        );
    }

    #[test]
    fn server_shutdown_tears_down_remote_focus_proxy_once() {
        let (mut server, operation_id, pane_id, terminal_id, detached) =
            server_with_remote_focus_proxy();

        server.initiate_shutdown();
        server.initiate_shutdown();

        assert_remote_focus_proxy_torn_down(
            &mut server,
            &operation_id,
            pane_id,
            &terminal_id,
            &detached,
        );
    }

    #[test]
    fn committed_live_handoff_tears_down_remote_focus_proxy_once() {
        let (mut server, operation_id, pane_id, terminal_id, detached) =
            server_with_remote_focus_proxy();

        server.finish_live_handoff_shutdown();
        server.finish_live_handoff_shutdown();

        assert_remote_focus_proxy_torn_down(
            &mut server,
            &operation_id,
            pane_id,
            &terminal_id,
            &detached,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn headless_loop_reconciles_remote_focus_transport_termination_without_event() {
        let (mut server, operation_id, pane_id, terminal_id, detached) =
            server_with_remote_focus_proxy();
        let operation_state = server
            .app
            .remote_focus_operations
            .operation_state(&operation_id)
            .expect("remote focus operation state");
        assert!(operation_state.terminate());

        // Let the test server stop only after the reconciliation path has
        // detached the proxy. A shutdown-triggered teardown cannot make this
        // test pass when the headless loop misses reconciliation.
        let should_quit = Arc::clone(&server.should_quit);
        let detached_for_watcher = Arc::clone(&detached);
        let watcher = tokio::spawn(async move {
            loop {
                if !detached_for_watcher
                    .lock()
                    .expect("detach recording lock")
                    .is_empty()
                {
                    should_quit.store(true, Ordering::Release);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });

        tokio::time::timeout(Duration::from_secs(2), server.run())
            .await
            .expect("headless server reconciles and exits")
            .expect("headless server run");
        watcher.await.expect("detach watcher");

        assert_remote_focus_proxy_torn_down_with_state(
            &mut server,
            &operation_id,
            pane_id,
            &terminal_id,
            &detached,
            api::schema::RemoteFocusState::Failed,
        );
    }

    #[test]
    fn a_fresh_attach_starts_from_the_config_diff_whitespace_choice() {
        let mut server = test_headless_server();
        server.app.state.dock_diff_ignore_whitespace = true;
        server.clients.insert(
            7,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                7,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        assert!(!server.clients[&7].dock_presentation.diff_ignore_whitespace);

        server.seed_client_dock_presentation(7);

        assert!(server.clients[&7].dock_presentation.diff_ignore_whitespace);
    }

    #[test]
    fn a_fresh_attach_uses_only_configured_panel_defaults() {
        let mut server = test_headless_server();
        server.app.state.dock_default_surfaces = vec![
            crate::app::DockSurface::Files,
            crate::app::DockSurface::Context,
        ];
        server.clients.insert(
            7,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                7,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        server.seed_client_dock_presentation(7);

        let presentation = &server.clients[&7].dock_presentation;
        assert_eq!(
            presentation.open_surfaces,
            vec![
                crate::app::DockSurface::Files,
                crate::app::DockSurface::Context
            ]
        );
        assert_eq!(presentation.tab, Some(crate::app::DockSurface::Files));
    }

    #[test]
    fn a_requested_refresh_runs_on_a_server_that_has_stopped_polling() {
        let mut server = test_headless_server();
        server.app.work_index_config.enabled = true;
        server.app.work_index_config.refresh_interval_seconds = 10;
        let now = Instant::now();
        server.last_app_client_seen = now - Duration::from_secs(61);
        assert!(!server.work_index_refresh_is_useful(now));

        // `day link` is the only thing that can complete the item, and nobody is
        // attached to carry the answer back. Swallowing it leaves a CLI-only user
        // reading `todo` for a merged pull request forever.
        server.app.work_index_refresh_requested = true;
        assert!(server.work_index_refresh_is_useful(now));

        // Running it clears the request, so the server goes back to sleep.
        server.app.next_work_index_refresh = now;
        server.app.start_work_index_refresh_if_due(now);
        assert!(!server.app.work_index_refresh_requested);
        assert!(!server.work_index_refresh_is_useful(now));
    }

    #[test]
    fn work_index_timer_skips_after_six_idle_intervals_and_attach_resumes_immediately() {
        let mut server = test_headless_server();
        server.app.work_index_config.enabled = true;
        server.app.work_index_config.refresh_interval_seconds = 10;
        let now = Instant::now();
        server.last_app_client_seen = now - Duration::from_secs(61);
        assert!(!server.work_index_refresh_is_useful(now));

        server.app.next_work_index_refresh = now + Duration::from_secs(30);
        let (writer, _control, _render) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 77,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));
        assert!(server.app.next_work_index_refresh <= Instant::now());
        assert!(server.work_index_refresh_is_useful(Instant::now()));
    }

    #[test]
    fn a_reloaded_diff_whitespace_choice_reaches_every_attach_and_drops_its_diff() {
        let mut server = test_headless_server();
        for client_id in [1, 2] {
            let mut client = ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                client_id,
                RenderEncoding::SemanticFrame,
                None,
            );
            client.dock_presentation.diff_active_key = Some(crate::app::state::DiffCacheKey {
                root: std::path::PathBuf::from("/repo"),
                base: "main".into(),
                ignore_whitespace: false,
            });
            server.clients.insert(client_id, client);
        }

        server.apply_dock_diff_whitespace_to_clients(true);

        for client_id in [1, 2] {
            let presentation = &server.clients[&client_id].dock_presentation;
            assert!(presentation.diff_ignore_whitespace);
            assert!(presentation.diff_active_key.is_none());
            assert!(presentation.diff_request.is_none());
        }
    }

    #[test]
    fn full_screen_work_view_requests_pr_details_without_open_dock() {
        let mut client = ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            crate::terminal_theme::TerminalTheme::default(),
            None,
            1,
            RenderEncoding::SemanticFrame,
            None,
        );
        let key = crate::app::state::WorkItemKey {
            repo: "owner/repo".into(),
            pr_number: Some(42),
            pr_url: Some("https://github.com/owner/repo/pull/42".into()),
            ticket_id: None,
        };
        let view = crate::app::state::WorkViewState::new(
            true,
            Some(crate::work_index::Snapshot {
                items: vec![crate::work_index::WorkItem {
                    repo: key.repo.clone(),
                    pr_number: key.pr_number,
                    pr_url: key.pr_url.clone(),
                    pr_title: Some("detail fallback".into()),
                    pr_state: Some("open".into()),
                    draft: false,
                    review_decision: None,
                    created_at: None,
                    updated_at: None,
                    additions: 0,
                    deletions: 0,
                    author: None,
                    assignees: Vec::new(),
                    labels: Vec::new(),
                    check_state: crate::work_index::PrCheckState::Unknown,
                    audience: crate::work_index::PrAudience::Authored,
                    cached_pr_detail: None,
                    ticket_ids: Vec::new(),
                    ticket_title: None,
                    ticket_state: None,
                    ticket_details: Vec::new(),
                    branch: None,
                    preview_urls: Vec::new(),
                    panes: Vec::new(),
                    source: crate::work_index::WorkItemSource::default(),
                }],
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: std::time::SystemTime::UNIX_EPOCH,
            }),
        );
        client.work_view = Some(view);

        assert_eq!(
            work_item_detail_request(&client),
            Some((crate::app::state::DockHomeSection::Prs, Some(key), true))
        );
    }

    #[test]
    fn focused_headless_linear_dock_requests_its_ticket_detail() {
        let mut client = test_app_client(Some(true), 1);
        client.dock_presentation.collapsed = false;
        client.dock_presentation.tab = Some(crate::app::DockSurface::Linear);
        client.dock_presentation.open_surfaces = vec![crate::app::DockSurface::Linear];
        client.dock_presentation.active_tab_index = Some(0);
        client.dock_presentation.tab_bindings = vec![Some(crate::app::state::DockTabBinding {
            object: crate::app::state::DockObjectRef {
                surface: crate::app::DockSurface::Linear,
                key: "SCA-3313".into(),
            },
            origin: crate::app::state::DockTabOrigin::Context,
        })];

        assert_eq!(
            work_item_detail_request(&client),
            Some((
                crate::app::state::DockHomeSection::Tickets,
                Some(crate::app::state::WorkItemKey {
                    repo: String::new(),
                    pr_number: None,
                    pr_url: None,
                    ticket_id: Some("SCA-3313".into()),
                }),
                true
            ))
        );
    }

    #[test]
    fn a_collapsed_linear_preview_requests_its_ticket_detail_too() {
        let mut client = test_app_client(Some(true), 1);
        client.dock_presentation.collapsed = true;
        client.dock_presentation.object_preview = Some(crate::app::state::DockObjectRef {
            surface: crate::app::DockSurface::Linear,
            key: "SCA-3313".into(),
        });

        assert_eq!(
            work_item_detail_request(&client),
            Some((
                crate::app::state::DockHomeSection::Tickets,
                Some(crate::app::state::WorkItemKey {
                    repo: String::new(),
                    pr_number: None,
                    pr_url: None,
                    ticket_id: Some("SCA-3313".into()),
                }),
                true
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn focused_headless_pr_dock_refreshes_bound_pr_detail() {
        use std::os::unix::fs::PermissionsExt;

        let mut server = test_headless_server();
        let fixture_dir = std::env::temp_dir().join(format!(
            "herdr-headless-pr-refresh-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&fixture_dir).expect("create fixture directory");
        let log = fixture_dir.join("detail.log");
        let gh = fixture_dir.join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
case "$*" in
  "pr view 42 --repo owner/repo --json "*)
    printf '%s\n' detail >> '{}'
    printf '%s' '{{"number":42,"title":"Detail","url":"https://github.com/owner/repo/pull/42"}}'
    ;;
  "api repos/owner/repo/issues/42/timeline"*) printf '%s' '[]' ;;
  "api graphql"*) printf '%s' '{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[]}}}}}}}}}}' ;;
  *) exit 42 ;;
esac
"#,
                log.display()
            ),
        )
        .expect("write fake gh");
        let mut permissions = std::fs::metadata(&gh)
            .expect("fake gh metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).expect("make fake gh executable");
        server.app.work_index_gh_program_override = Some(gh);
        server.app.work_index_provider_cache_root_override = Some(fixture_dir.clone());

        let mut client = test_app_client(Some(true), 1);
        client.dock_presentation.collapsed = false;
        client.dock_presentation.tab = Some(crate::app::DockSurface::Pr);
        client.dock_presentation.open_surfaces = vec![crate::app::DockSurface::Pr];
        client.dock_presentation.active_tab_index = Some(0);
        client.dock_presentation.tab_bindings = vec![Some(crate::app::state::DockTabBinding {
            object: crate::app::state::DockObjectRef {
                surface: crate::app::DockSurface::Pr,
                key: "https://github.com/owner/repo/pull/42".into(),
            },
            origin: crate::app::state::DockTabOrigin::Context,
        })];
        let key = crate::app::state::WorkItemKey {
            repo: "owner/repo".into(),
            pr_number: Some(42),
            pr_url: Some("https://github.com/owner/repo/pull/42".into()),
            ticket_id: None,
        };
        assert_eq!(
            work_item_detail_request(&client),
            Some((
                crate::app::state::DockHomeSection::Prs,
                Some(key.clone()),
                true
            ))
        );
        let (writer, _control_rx, _render_rx) = test_client_writer();
        client.writer = Some(writer);
        server.clients.insert(1, client);
        server.foreground_client_id = Some(1);

        server.handle_scheduled_tasks_headless(Instant::now(), false);

        let event = server
            .app
            .event_rx
            .blocking_recv()
            .expect("headless detail refresh result");
        let crate::events::AppEvent::WorkItemDetailRefreshed {
            generation,
            details,
        } = event
        else {
            panic!("expected detail refresh event");
        };
        assert!(server
            .app
            .handle_work_item_detail_refreshed(generation, details));
        assert_eq!(
            std::fs::read_to_string(log).expect("read detail counter"),
            "detail\n"
        );
        assert!(server.app.state.work_item_detail_cache.get(&key).is_some());
        let _ = std::fs::remove_dir_all(fixture_dir);
    }

    #[tokio::test]
    async fn pre_render_status_context_matches_headless_client_focus_mutation() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("headless-status-focus");
        let first_tab = workspace.active_tab;
        let first_pane = workspace.tabs[first_tab].root_pane;
        let second_tab = workspace.test_add_tab(Some("second"));
        let second_pane = workspace.tabs[second_tab].root_pane;
        workspace.switch_tab(first_tab);
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.ensure_test_terminals();
        let first_terminal = server.app.state.workspaces[0]
            .terminal_id(first_pane)
            .expect("first terminal")
            .clone();
        let second_terminal = server.app.state.workspaces[0]
            .terminal_id(second_pane)
            .expect("second terminal")
            .clone();
        server
            .app
            .state
            .terminals
            .get_mut(&first_terminal)
            .unwrap()
            .cwd = "/first".into();
        server
            .app
            .state
            .terminals
            .get_mut(&second_terminal)
            .unwrap()
            .cwd = "/second".into();
        server.app.state.sync_status_focused_cached_cwd();
        server.app.state.status_git_cwd = Some("/first".into());
        server.app.state.status_git_branch = Some("first-branch".into());

        server.app.route_client_input(vec![0x02, b'n']);
        assert_eq!(server.app.state.workspaces[0].active_tab, second_tab);
        let (stale_cwd, stale_branch) =
            crate::ui::focused_status_context_for_test(&server.app.state);
        assert_eq!(stale_cwd, Some(std::path::PathBuf::from("/first")));
        assert_eq!(stale_branch.as_deref(), Some("first-branch"));

        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();
        assert!(server.app.sync_status_context_before_render());
        server.render_and_stream();
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&server.app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/second")));
        assert_eq!(branch, None);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
    }

    fn shutdown_test_runtimes(server: &mut HeadlessServer) {
        for (_, runtime) in server.app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    fn read_server_message(bytes: Vec<u8>) -> ServerMessage {
        let mut cursor = std::io::Cursor::new(bytes);
        protocol::read_message(&mut cursor, MAX_FRAME_SIZE).expect("decode server message")
    }

    fn read_server_frame(bytes: Vec<u8>) -> FrameData {
        match protocol::read_message(&mut std::io::Cursor::new(bytes), MAX_GRAPHICS_FRAME_SIZE)
            .expect("decode server frame")
        {
            ServerMessage::Frame(frame) => frame,
            other => panic!("expected frame, got {other:?}"),
        }
    }

    fn frame_text(frame: &FrameData) -> String {
        frame
            .cells
            .chunks(usize::from(frame.width))
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn notepad_projection_is_rendered_from_each_attached_clients_presentation() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("alpha")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.ensure_test_terminals();
        let pane_id = server.app.state.workspaces[0].focused_pane_id().unwrap();
        let terminal_id = server.app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Codex),
                crate::detect::AgentState::Working,
            );
        for index in 0..30 {
            server.app.state.agent_states.observe_links(
                pane_id,
                [format!("https://github.com/owner/repo/pull/{index}")],
                crate::agent_state::AgentLinkSource::Output,
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000 + index),
            );
        }
        server.app.state.notepad.enabled = true;
        server.app.state.notepad.height = 18;
        server
            .app
            .state
            .notepad
            .set_files(vec![crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            }]);
        server.app.state.notepad.set_body("note");
        server.app.state.provider_usage =
            crate::provider_usage::ProviderUsageSnapshot::with_primary_accounts(
                crate::provider_usage::AccountUsage::default(),
                crate::provider_usage::AccountUsage::default(),
                crate::provider_usage::AccountUsage::default(),
            );
        let template = server.app.state.provider_usage.accounts[0].clone();
        for index in 0..20 {
            let mut account = template.clone();
            account.profile_id = format!("extra-{index}");
            account.label = format!("extra-{index}");
            server.app.state.provider_usage.accounts.push(account);
        }

        // Each client selects a different tab, so all of them must be visible.
        let mut visible_tabs = server.app.state.notepad.visible_tabs.clone();
        for tab in ["note:todo", "agent", "usage"] {
            if !visible_tabs.iter().any(|visible| visible == tab) {
                visible_tabs.push(tab.to_string());
            }
        }
        server.app.state.notepad.set_visible_tabs(visible_tabs);
        let (first_writer, _first_control, first_render) = test_client_writer();
        let (second_writer, _second_control, second_render) = test_client_writer();
        let (third_writer, _third_control, third_render) = test_client_writer();
        let mut first = ClientConnection::new(
            (120, 40),
            crate::kitty_graphics::HostCellSize::default(),
            crate::terminal_theme::TerminalTheme::default(),
            Some(true),
            2,
            RenderEncoding::SemanticFrame,
            Some(first_writer),
        );
        first.notepad_presentation.agent_tab = true;
        first.notepad_presentation.agent_collapsed.tasks = true;
        first.notepad_presentation.agent_scroll = 1;
        let mut second = ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            crate::terminal_theme::TerminalTheme::default(),
            Some(false),
            1,
            RenderEncoding::SemanticFrame,
            Some(second_writer),
        );
        second.notepad_presentation.usage_tab = true;
        second.notepad_presentation.usage_scroll = 1;
        let third = ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            crate::terminal_theme::TerminalTheme::default(),
            Some(false),
            0,
            RenderEncoding::SemanticFrame,
            Some(third_writer),
        );
        server.clients.insert(1, first);
        server.clients.insert(2, second);
        server.clients.insert(3, third);
        server.foreground_client_id = Some(1);
        server.app.full_redraw_pending = true;

        server.render_and_stream();

        let first_text = frame_text(&read_server_frame(
            first_render
                .recv_timeout(Duration::from_secs(1))
                .expect("first client frame"),
        ));
        let second_text = frame_text(&read_server_frame(
            second_render
                .recv_timeout(Duration::from_secs(1))
                .expect("second client frame"),
        ));
        let third_text = frame_text(&read_server_frame(
            third_render
                .recv_timeout(Duration::from_secs(1))
                .expect("third client frame"),
        ));
        assert!(first_text.contains("▸ Tasks"), "{first_text}");
        assert!(!first_text.contains("note"), "{first_text}");
        assert!(second_text.contains("CCE 5"), "{second_text}");
        assert!(!second_text.contains("note"), "{second_text}");
        assert!(third_text.contains("note"), "{third_text}");
        assert!(
            server.clients[&1]
                .notepad_presentation
                .agent_collapsed
                .tasks
        );
        assert!(server.clients[&2].notepad_presentation.usage_tab);
        assert!(!server.clients[&3].notepad_presentation.agent_tab);
        assert_eq!(server.clients[&1].notepad_presentation.agent_scroll, 1);
        assert_eq!(server.clients[&2].notepad_presentation.usage_scroll, 1);
    }

    fn read_server_shutdown_reason(bytes: Vec<u8>) -> Option<String> {
        match read_server_message(bytes) {
            ServerMessage::ServerShutdown { reason } => reason,
            other => panic!("expected shutdown, got {other:?}"),
        }
    }

    #[test]
    fn default_headless_size_is_effective_without_clients() {
        let server = test_headless_server();

        assert_eq!(
            server.headless_size,
            (
                crate::config::DEFAULT_HEADLESS_COLS,
                crate::config::DEFAULT_HEADLESS_ROWS
            )
        );
        assert_eq!(server.effective_size, server.headless_size);
    }

    #[tokio::test]
    async fn headless_api_reads_latest_title_without_spinner_event_flooding() {
        let event_hub = api::EventHub::default();
        let mut server = test_headless_server_with_event_hub(event_hub.clone());
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.sidebar_width = 30;
        server.app.state.sidebar_agents.rows = vec![vec![
            crate::config::AgentSidebarToken::TerminalTitleStripped,
        ]];
        let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = server.app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(crate::detect::Agent::Claude);
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes(b"\x1b]0;\xe2\xa0\x8b task\x07");
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        server.app.render_dirty.request_terminal_title(pane_id);

        let first = headless_pane_list(&mut server).pop().unwrap();
        assert_eq!(first.terminal_title.as_deref(), Some("⠋ task"));
        assert_eq!(first.terminal_title_stripped.as_deref(), Some("task"));
        assert_eq!(pane_updated_events(&event_hub), 1);
        let (buffer, _) = crate::server::render_stream::render_virtual_with_runtime_registry(
            &mut server.app.state,
            &server.app.terminal_runtimes,
            Rect::new(0, 0, 100, 30),
            true,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            rendered.contains("task"),
            "agent terminal titles drive the tab label: {rendered:?}"
        );
        assert!(
            !rendered.contains("⠋ task"),
            "spinner decorations must be stripped before the title reaches the sidebar: {rendered:?}"
        );

        server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]2;\xe2\xa0\x99 task\x1b\\");
        server.app.render_dirty.request_terminal_title(pane_id);
        let second = headless_pane_list(&mut server).pop().unwrap();
        assert_eq!(second.terminal_title.as_deref(), Some("⠙ task"));
        assert_eq!(second.terminal_title_stripped.as_deref(), Some("task"));
        assert_eq!(pane_updated_events(&event_hub), 1);
    }

    fn headless_pane_list(server: &mut HeadlessServer) -> Vec<api::schema::PaneInfo> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "list-titles".into(),
                method: api::schema::Method::PaneList(api::schema::PaneListParams::default()),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });
        let response: api::schema::SuccessResponse =
            serde_json::from_str(&response_rx.recv().unwrap()).unwrap();
        let api::schema::ResponseResult::PaneList { panes } = response.result else {
            panic!("expected pane list");
        };
        panes
    }

    fn pane_updated_events(event_hub: &api::EventHub) -> usize {
        event_hub
            .events_after(0)
            .iter()
            .filter(|(_, event)| event.event == api::schema::EventKind::PaneUpdated)
            .count()
    }

    #[test]
    fn server_stop_interrupts_server_event_backlog() {
        let mut server = test_headless_server();
        for client_id in 1..=64 {
            server
                .server_event_tx
                .try_send(ServerEvent::ClientDisconnected { client_id })
                .unwrap();
        }

        server.should_quit.store(true, Ordering::Release);

        assert!(!server.drain_server_events());
        assert!(server.server_event_rx.try_recv().is_ok());
        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn headless_api_request_drains_all_pending_internal_events_before_reading_state() {
        let mut server = test_headless_server();
        for i in 0..=crate::app::APP_EVENT_DRAIN_LIMIT {
            server
                .app
                .event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("4.0.{i}"),
                    install_command: "herdr install".into(),
                })
                .unwrap();
        }

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "headless_stop_after_events".into(),
                    method: api::schema::Method::ServerStop(api::schema::EmptyParams::default()),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        let expected_version = format!("4.0.{}", crate::app::APP_EVENT_DRAIN_LIMIT);
        assert_eq!(
            server.app.state.update_available.as_deref(),
            Some(expected_version.as_str())
        );
        assert!(server.app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn headless_deferred_workspace_create_uses_runtime_events() {
        let event_hub = api::EventHub::default();
        let mut server = test_headless_server_with_event_hub(event_hub.clone());

        server.app.state.request_new_workspace = true;

        assert!(server.handle_deferred_requests_headless());
        assert!(!server.app.state.request_new_workspace);
        assert_eq!(
            event_hub
                .events_after(0)
                .into_iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            vec![
                api::schema::EventKind::WorkspaceCreated,
                api::schema::EventKind::TabCreated,
                api::schema::EventKind::PaneCreated,
                api::schema::EventKind::LayoutUpdated,
            ]
        );
        shutdown_test_runtimes(&mut server);
    }

    #[tokio::test]
    async fn headless_deferred_named_tab_create_uses_runtime_events() {
        let event_hub = api::EventHub::default();
        let mut server = test_headless_server_with_event_hub(event_hub.clone());
        server
            .app
            .create_workspace_with_options(std::env::temp_dir(), true)
            .unwrap();
        let after_setup = event_hub.current_sequence();

        server.app.state.request_new_tab = true;
        server.app.state.requested_new_tab_name = Some("ops".into());

        assert!(server.handle_deferred_requests_headless());
        assert!(!server.app.state.request_new_tab);
        assert_eq!(server.app.state.requested_new_tab_name, None);
        let events = event_hub.events_after(after_setup);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            vec![
                api::schema::EventKind::TabCreated,
                api::schema::EventKind::PaneCreated,
                api::schema::EventKind::LayoutUpdated,
            ]
        );
        let tab_created = events
            .iter()
            .find_map(|(_, event)| match &event.data {
                api::schema::EventData::TabCreated { tab } => Some(tab),
                _ => None,
            })
            .expect("tab created event");
        assert_eq!(tab_created.label, "ops");
        shutdown_test_runtimes(&mut server);
    }

    fn window_title_test_server() -> (HeadlessServer, std::sync::mpsc::Receiver<Vec<u8>>) {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("herd")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;

        let (client_tx, control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.promote_client_to_foreground(1);
        drain_window_titles(&control_rx);
        (server, control_rx)
    }

    /// The test client writer drains its queue on a background thread, so
    /// reading a pushed message needs a timeout rather than `try_recv`.
    fn next_window_title(
        control_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) -> Option<Option<String>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let Ok(bytes) = control_rx.recv_timeout(remaining) else {
                return None;
            };
            if let ServerMessage::WindowTitle { title } = read_server_message(bytes) {
                return Some(title);
            }
        }
        None
    }

    fn drain_window_titles(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>) {
        while control_rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
    }

    fn no_window_title(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> bool {
        while let Ok(bytes) = control_rx.recv_timeout(Duration::from_millis(200)) {
            if let ServerMessage::WindowTitle { .. } = read_server_message(bytes) {
                return false;
            }
        }
        true
    }

    #[test]
    fn window_title_waits_for_a_foreground_client_to_exist() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("herd")];
        server.app.state.active = Some(0);
        server.app.configure_window_title("{workspace}");

        // The server renders before the first client attaches. Nothing was
        // delivered, so nothing may be recorded as delivered either.
        server.sync_window_title();
        assert_eq!(server.sent_window_title, None);

        let (client_tx, control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.promote_client_to_foreground(1);
        server.sync_window_title();

        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("herd".to_string()))
        );
        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn an_attaching_client_gets_the_title_even_when_it_has_not_changed() {
        let (mut server, first_control_rx) = window_title_test_server();
        server.app.configure_window_title("{workspace}");
        server.sync_window_title();
        assert_eq!(
            next_window_title(&first_control_rx),
            Some(Some("herd".to_string()))
        );

        // ClientConnected assigns the foreground client directly rather than
        // going through promote_client_to_foreground, so the cache must notice
        // the new client on its own.
        let (client_tx, second_control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_window_title();

        assert_eq!(
            next_window_title(&second_control_rx),
            Some(Some("herd".to_string()))
        );
        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn configured_window_title_reaches_the_foreground_client_once_per_change() {
        let (mut server, control_rx) = window_title_test_server();
        server.app.configure_window_title("{workspace}/{tab}");

        server.sync_window_title();
        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("herd/1".to_string()))
        );

        // An unchanged title must not re-emit an OSC on every render.
        server.sync_window_title();
        assert!(no_window_title(&control_rx));

        server.app.state.workspaces[0].tabs[0].custom_name = Some("build".into());
        server.sync_window_title();
        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("herd/build".to_string()))
        );

        shutdown_test_runtimes(&mut server);
    }

    #[tokio::test]
    async fn focused_terminal_title_syncs_without_requesting_a_sidebar_render() {
        let (mut server, control_rx) = window_title_test_server();
        server.app.configure_window_title("{terminal_title}");
        server.app.state.ensure_test_terminals();
        let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = server.app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal")
            .clone();
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes("\x1b]0;⠋ building\x07".as_bytes());
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);

        assert_eq!(
            server.sync_terminal_title_sources(&HashSet::from([pane_id])),
            (false, true)
        );
        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("building".to_string()))
        );

        server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .test_process_pty_bytes("\x1b]0;⠙ building\x07".as_bytes());
        assert_eq!(
            server.sync_terminal_title_sources(&HashSet::from([pane_id])),
            (false, true)
        );
        assert!(no_window_title(&control_rx));

        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn a_foreground_client_without_a_writer_does_not_cache_the_window_title() {
        let (mut server, _control_rx) = window_title_test_server();
        server.app.configure_window_title("{workspace}");

        // A detached client keeps its entry but loses its writer, so nothing
        // reaches a terminal even though the targeted send reports success.
        if let Some(client) = server.clients.get_mut(&1) {
            client.writer = None;
        }
        server.sync_window_title();
        assert!(server.sent_window_title.is_none());

        // Attaching again has to deliver the title rather than skip it as sent.
        let (client_tx, control_rx, _render_rx) = test_client_writer();
        if let Some(client) = server.clients.get_mut(&1) {
            client.writer = Some(client_tx);
        }
        server.sync_window_title();
        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("herd".to_string()))
        );

        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn empty_window_title_config_leaves_the_outer_title_alone() {
        let (mut server, control_rx) = window_title_test_server();
        server.app.configure_window_title("");

        server.sync_window_title();

        assert!(no_window_title(&control_rx));
        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn api_window_title_wins_until_it_is_cleared() {
        let (mut server, control_rx) = window_title_test_server();
        server.app.configure_window_title("{workspace}");

        server.handle_client_window_title_api("set".into(), Some("herdr api".into()));
        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("herdr api".to_string()))
        );

        server.app.state.workspaces[0].custom_name = Some("ops".into());
        server.sync_window_title();
        assert!(no_window_title(&control_rx));

        // Clearing hands the title back to ui.window_title, not to "herdr".
        server.handle_client_window_title_api("clear".into(), None);
        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("ops".to_string()))
        );

        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn clearing_the_api_title_falls_back_to_herdr_when_window_titles_are_disabled() {
        let (mut server, control_rx) = window_title_test_server();
        server.app.configure_window_title("");

        server.handle_client_window_title_api("set".into(), Some("herdr api".into()));
        assert_eq!(
            next_window_title(&control_rx),
            Some(Some("herdr api".to_string()))
        );

        server.handle_client_window_title_api("clear".into(), None);
        assert_eq!(next_window_title(&control_rx), Some(None));

        shutdown_test_runtimes(&mut server);
    }

    #[test]
    fn a_newly_promoted_client_gets_the_window_title_again() {
        let (mut server, first_control_rx) = window_title_test_server();
        server.app.configure_window_title("{workspace}");
        server.sync_window_title();
        assert_eq!(
            next_window_title(&first_control_rx),
            Some(Some("herd".to_string()))
        );

        // A second terminal starts on whatever its shell or ssh left behind.
        let (client_tx, second_control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.promote_client_to_foreground(2);
        server.sync_window_title();

        assert_eq!(
            next_window_title(&second_control_rx),
            Some(Some("herd".to_string()))
        );
        shutdown_test_runtimes(&mut server);
    }

    fn test_client_writer() -> (
        ClientWriter,
        std::sync::mpsc::Receiver<Vec<u8>>,
        std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let (control_tx, control_rx) = std::sync::mpsc::channel();
        let (render_tx, render_rx) = std::sync::mpsc::sync_channel(1);
        (
            ClientWriter::test_channel(control_tx, render_tx),
            control_rx,
            render_rx,
        )
    }

    fn acknowledge_latest_test_render(server: &mut HeadlessServer, client_id: u64) {
        let render_sequence = server.clients[&client_id]
            .latest_pending_pomodoro_render_sequence()
            .expect("pending test render");
        let _ = server.handle_server_event(ServerEvent::ClientWriterDrained {
            client_id,
            render_sequence,
        });
    }

    fn retained_test_server(
        initial_screen: &[u8],
    ) -> (
        HeadlessServer,
        std::sync::mpsc::Receiver<Vec<u8>>,
        crate::layout::PaneId,
    ) {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        workspace.id = "w1".into();
        let pane_id = workspace.focused_pane_id().expect("focused pane");
        workspace.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, initial_screen),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let (client_tx, _client_control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        (server, client_rx, pane_id)
    }

    fn client_key(
        server: &mut HeadlessServer,
        client_id: u64,
        code: crate::protocol::ClientKeyCode,
        modifiers: KeyModifiers,
    ) {
        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code,
                modifiers: modifiers.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));
    }

    fn client_prefix_action(
        server: &mut HeadlessServer,
        client_id: u64,
        code: crate::protocol::ClientKeyCode,
        modifiers: KeyModifiers,
    ) {
        client_key(
            server,
            client_id,
            crate::protocol::ClientKeyCode::Char('b'),
            KeyModifiers::CONTROL,
        );
        client_key(server, client_id, code, modifiers);
    }

    fn open_client_editor_preview(
        server: &mut HeadlessServer,
        client_id: u64,
        client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let root = PathBuf::from("/nonexistent/herdr-retained-preview");
        server.app.state.dock_files_root = Some(root.clone());
        server.app.state.dock_file_cache.insert(
            root.clone(),
            crate::files::FileTreeSnapshot {
                root,
                files: vec![crate::files::FileRecord {
                    path: PathBuf::from("preview.rs"),
                    status: None,
                    kind: crate::files::FileTreeRowKind::File,
                }],
                fingerprint: 1,
                source: crate::files::FileTreeSource::Git,
                error: None,
            },
        );
        let dock = &mut server
            .clients
            .get_mut(&client_id)
            .expect("client")
            .dock_presentation;
        dock.collapsed = false;
        dock.tab = Some(crate::app::DockSurface::Files);
        dock.open_surfaces = vec![crate::app::DockSurface::Files];
        dock.tab_bindings = vec![None];
        dock.active_tab_index = Some(0);
        dock.files_focused = true;

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("files surface frame");
        let hit = server
            .app
            .state
            .view
            .dock_file_row_hit_areas
            .first()
            .expect("preview file row")
            .rect;
        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id,
            events: vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Down(
                    crate::protocol::ClientMouseButton::Left,
                ),
                column: hit.x,
                row: hit.y,
                modifiers: 0,
            }],
        }));
    }

    fn open_client_symphony(
        server: &mut HeadlessServer,
        client_id: u64,
        _client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        client_prefix_action(
            server,
            client_id,
            crate::protocol::ClientKeyCode::Char('s'),
            KeyModifiers::SHIFT,
        );
    }

    fn open_client_loop_history(
        server: &mut HeadlessServer,
        client_id: u64,
        _client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        client_prefix_action(
            server,
            client_id,
            crate::protocol::ClientKeyCode::Char('h'),
            KeyModifiers::CONTROL,
        );
    }

    fn open_client_usage(
        server: &mut HeadlessServer,
        client_id: u64,
        _client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        client_prefix_action(
            server,
            client_id,
            crate::protocol::ClientKeyCode::Char('y'),
            KeyModifiers::CONTROL,
        );
    }

    fn open_client_work(
        server: &mut HeadlessServer,
        client_id: u64,
        _client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        client_prefix_action(
            server,
            client_id,
            crate::protocol::ClientKeyCode::Char('w'),
            KeyModifiers::CONTROL,
        );
    }

    fn open_client_dock_object_preview(
        server: &mut HeadlessServer,
        client_id: u64,
        _client_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let dock = &mut server
            .clients
            .get_mut(&client_id)
            .expect("client")
            .dock_presentation;
        dock.collapsed = true;
        dock.object_preview = Some(crate::app::state::DockObjectRef {
            surface: crate::app::DockSurface::Linear,
            key: "SCA-1".into(),
        });
    }

    #[tokio::test]
    async fn presented_pomodoro_overlays_reject_retained_pty_updates() {
        for overlay in ["prompt", "send-off"] {
            let (mut server, client_rx, pane_id) = retained_test_server(b"tiled pane");
            let now = Instant::now();
            match overlay {
                "prompt" => {
                    server.app.state.pomodoro.prompt = Some(crate::pomodoro::PomodoroPrompt {
                        ended: crate::pomodoro::PomodoroPhase::Work,
                        next: crate::pomodoro::PomodoroPhase::ShortBreak,
                        raised_at: now,
                        input: String::new(),
                        error: None,
                    });
                }
                "send-off" => {
                    server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
                        started: crate::pomodoro::PomodoroPhase::ShortBreak,
                        shown_at: now,
                    });
                }
                _ => unreachable!(),
            }
            server.render_and_stream();
            let overlay_frame = read_server_frame(
                client_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("overlay frame"),
            );
            let presentation = server.clients[&1].pomodoro_presentation;
            assert!(
                presentation.prompt.is_some() || presentation.send_off.is_some(),
                "{overlay} must own input"
            );
            let runtime = server
                .app
                .state
                .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
                .expect("runtime");
            runtime.test_process_pty_bytes(b"\rZ");

            assert!(
                !server.render_retained_pty_update_and_stream(),
                "{overlay} accepted a retained pane patch"
            );
            assert_frame_data_eq(
                server.clients[&1]
                    .render_state
                    .last_frame()
                    .expect("overlay frame retained"),
                &overlay_frame,
            );
            assert!(
                client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
                "{overlay} received a retained pane patch"
            );
        }
    }

    #[tokio::test]
    async fn attach_local_surfaces_reject_tiled_pane_retained_updates() {
        type SurfaceSetup = (
            &'static str,
            fn(&mut HeadlessServer, u64, &std::sync::mpsc::Receiver<Vec<u8>>),
        );
        let setups: [SurfaceSetup; 6] = [
            ("editor preview", open_client_editor_preview),
            ("symphony", open_client_symphony),
            ("loop history", open_client_loop_history),
            ("usage", open_client_usage),
            ("work", open_client_work),
            ("dock object preview", open_client_dock_object_preview),
        ];

        for (name, setup) in setups {
            let (mut server, client_rx, pane_id) = retained_test_server(b"tiled pane");
            server.render_and_stream();
            let _ = client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial tab frame");
            setup(&mut server, 1, &client_rx);
            assert!(
                !server.clients[&1]
                    .presentation_policy(&server.app.state)
                    .tab_surface_visible(),
                "{name}"
            );
            server.render_and_stream();
            let surface_frame = read_server_frame(
                client_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("replacement surface frame"),
            );
            let runtime = server
                .app
                .state
                .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
                .expect("runtime");
            runtime.test_process_pty_bytes(b"\rZ");

            assert!(server.render_retained_pty_update_and_stream(), "{name}");
            assert_frame_data_eq(
                server.clients[&1]
                    .render_state
                    .last_frame()
                    .expect("replacement frame retained"),
                &surface_frame,
            );
            assert!(
                client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
                "{name} received a tiled-pane patch"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_attach_resize_does_not_resize_dock_editor_runtime() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.focused_pane_id().expect("focused pane");
        let agent_terminal_id = workspace
            .terminal_id(pane_id)
            .expect("agent terminal id")
            .clone();
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.ensure_test_terminals();
        server
            .app
            .state
            .terminals
            .get_mut(&agent_terminal_id)
            .expect("agent terminal state")
            .set_detected_state(
                Some(crate::detect::Agent::Codex),
                crate::detect::AgentState::Idle,
            );
        server.app.state.dock_collapsed = false;
        server.app.state.dock_tab = Some(crate::app::DockSurface::Editor);

        let editor_terminal_id = crate::terminal::TerminalId::alloc();
        server.app.state.dock_editor_sessions.insert(
            pane_id,
            crate::app::state::DockEditorSession {
                pane_id: crate::layout::PaneId::alloc(),
                terminal_id: editor_terminal_id.clone(),
            },
        );
        server.app.terminal_runtimes.insert(
            editor_terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 2, b"EDITOR"),
        );
        let before = server
            .app
            .terminal_runtimes
            .get(&editor_terminal_id)
            .expect("editor runtime")
            .current_size();

        let (client_tx, _control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new_with_mode(
                ClientConnectionMode::TerminalAttach {
                    terminal_id: agent_terminal_id.to_string(),
                    control: None,
                },
                None,
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                false,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.resize_shared_runtime_to_effective_size();

        let after = server
            .app
            .terminal_runtimes
            .get(&editor_terminal_id)
            .expect("editor runtime")
            .current_size();
        assert_eq!(after, before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_app_resize_keeps_host_cell_size_when_graphics_are_disabled() {
        let mut server = test_headless_server();
        assert!(!server.app.state.kitty_graphics_enabled);

        let mut workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.root_pane;
        let background_tab = workspace.test_add_tab(Some("background"));
        let background_pane_id = workspace.tabs[background_tab].root_pane;
        workspace.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b""),
        );
        workspace.tabs[background_tab].runtimes.insert(
            background_pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b""),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                },
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        for (tab, pane_id) in [(0, pane_id), (background_tab, background_pane_id)] {
            let runtime = &server.app.state.workspaces[0].tabs[tab].runtimes[&pane_id];
            let (rows, cols) = runtime.current_size();
            assert_eq!(
                runtime.pixel_size(),
                Some((u32::from(cols) * 10, u32::from(rows) * 20))
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_app_background_client_does_not_resize_shared_dock_editor_runtime() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.focused_pane_id().expect("focused pane");
        let agent_terminal_id = workspace
            .terminal_id(pane_id)
            .expect("agent terminal id")
            .clone();
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.ensure_test_terminals();
        server
            .app
            .state
            .terminals
            .get_mut(&agent_terminal_id)
            .expect("agent terminal state")
            .set_detected_state(
                Some(crate::detect::Agent::Codex),
                crate::detect::AgentState::Idle,
            );

        let editor_terminal_id = crate::terminal::TerminalId::alloc();
        server.app.state.dock_editor_sessions.insert(
            pane_id,
            crate::app::state::DockEditorSession {
                pane_id: crate::layout::PaneId::alloc(),
                terminal_id: editor_terminal_id.clone(),
            },
        );
        server.app.terminal_runtimes.insert(
            editor_terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 2, b"EDITOR"),
        );
        let before = server
            .app
            .terminal_runtimes
            .get(&editor_terminal_id)
            .expect("editor runtime")
            .current_size();
        let before_resize_count = server
            .app
            .terminal_runtimes
            .get(&editor_terminal_id)
            .expect("editor runtime")
            .test_resize_count();

        let mut render_receivers = Vec::new();
        for (client_id, terminal_size, dock_width, editor_focused) in [
            (1_u64, (100, 30), 24_u16, true),
            (2_u64, (160, 45), 48_u16, true),
        ] {
            let (client_tx, _control_rx, render_rx) = test_client_writer();
            render_receivers.push(render_rx);
            let mut client = ClientConnection::new(
                terminal_size,
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                client_id,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            );
            client.dock_presentation = crate::app::state::DockPresentationState {
                surface_override: true,
                width: dock_width,
                collapsed: false,
                tab: Some(crate::app::DockSurface::Editor),
                open_surfaces: vec![crate::app::DockSurface::Editor],
                tab_bindings: vec![None],
                active_tab_index: Some(0),
                hovered_control: None,
                hover_started_at: None,
                hover_tooltip_visible: false,
                pane_tabs: std::collections::HashMap::new(),
                followed_pane: None,
                context_objects: Vec::new(),
                suppressed_context: std::collections::HashSet::new(),
                maximized: false,
                surface_menu: None,
                chooser_focused: false,
                scroll: 0,
                object_preview: None,
                object_views: std::collections::HashMap::new(),
                editor_focused,
                editor_preview: None,
                diff_focused: false,
                pr_focused: false,
                pr_checkout_menu: None,
                pr_action_menu: None,
                pr_action_confirmation: None,
                diff_ignore_whitespace: false,
                diff_selected: 0,
                diff_collapsed: std::collections::HashSet::new(),
                diff_request: None,
                diff_active_key: None,
                files_focused: false,
                files_selection: None,
                files_filter: String::new(),
                files_collapsed: std::collections::HashSet::new(),
                files_sort: crate::files::FileSort::Name,
                files_search_active: false,
                files_last_click: None,
                agents_focused: false,
                agents_selection: None,
                hosts_focused: false,
                hosts_selection: None,
                linear_focused: false,
                ticket_start_menu: None,
                ticket_action_menu: None,
                ticket_comment_draft: None,
                comment_draft: None,
                pending_write: None,
                write_notice: None,
                home_selection: None,
                home_ticket_selection: None,
                home_poll_selection: None,
                home_section: crate::app::state::DockHomeSection::Prs,
                home_detail_tab: crate::app::state::DockHomeDetailTab::Overview,
                home_focused: false,
                home_followed_pane: None,
                symphony: None,
            };
            server.clients.insert(client_id, client);
        }
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.render_and_stream();

        let after = server
            .app
            .terminal_runtimes
            .get(&editor_terminal_id)
            .expect("editor runtime")
            .current_size();
        let after_resize_count = server
            .app
            .terminal_runtimes
            .get(&editor_terminal_id)
            .expect("editor runtime")
            .test_resize_count();
        assert_eq!(
            after_resize_count,
            before_resize_count + 1,
            "background client resized shared editor PTY: before_size={before:?}, after_size={after:?}, before_resize_count={before_resize_count}, after_resize_count={after_resize_count}"
        );
        drop(render_receivers);
        shutdown_test_runtimes(&mut server);
    }

    fn hidden_pty_visibility_test_server(
        client_sizes: &[(u16, u16)],
    ) -> (HeadlessServer, crate::layout::PaneId) {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let background_tab = workspace.test_add_tab(Some("background"));
        let background_pane = workspace.tabs[background_tab].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        for (index, &terminal_size) in client_sizes.iter().enumerate() {
            let client_id = index as u64 + 1;
            let (client_tx, _client_control_rx, _client_rx) = test_client_writer();
            server.clients.insert(
                client_id,
                ClientConnection::new(
                    terminal_size,
                    crate::kitty_graphics::HostCellSize::default(),
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    client_id,
                    RenderEncoding::SemanticFrame,
                    Some(client_tx),
                ),
            );
        }

        (server, background_pane)
    }

    fn assert_frame_data_eq(actual: &FrameData, expected: &FrameData) {
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height)
        );
        assert_eq!(actual.cursor, expected.cursor, "cursor mismatch");
        assert_eq!(actual.hyperlinks, expected.hyperlinks, "hyperlink mismatch");
        assert_eq!(actual.graphics, expected.graphics, "graphics mismatch");
        assert_eq!(
            actual.cells.len(),
            expected.cells.len(),
            "cell length mismatch"
        );
        for (idx, (actual_cell, expected_cell)) in
            actual.cells.iter().zip(expected.cells.iter()).enumerate()
        {
            if cells_equivalent_for_frame_compare(
                &actual.cells,
                &expected.cells,
                usize::from(actual.width),
                idx,
                actual_cell,
                expected_cell,
            ) {
                continue;
            }
            assert_eq!(
                actual_cell,
                expected_cell,
                "cell mismatch at index {idx} (x={}, y={})",
                idx % usize::from(actual.width),
                idx / usize::from(actual.width),
            );
        }
    }

    fn cells_equivalent_for_frame_compare(
        actual_cells: &[CellData],
        expected_cells: &[CellData],
        width: usize,
        idx: usize,
        actual: &CellData,
        expected: &CellData,
    ) -> bool {
        if actual == expected {
            return true;
        }
        if !cell_style_without_symbol_eq(actual, expected) {
            return false;
        }
        if !matches!(
            (actual.symbol.as_str(), expected.symbol.as_str()),
            ("", " ") | (" ", "")
        ) {
            return false;
        }
        covered_by_previous_wide_cell(actual_cells, width, idx)
            || covered_by_previous_wide_cell(expected_cells, width, idx)
    }

    fn cell_style_without_symbol_eq(a: &CellData, b: &CellData) -> bool {
        a.fg == b.fg
            && a.bg == b.bg
            && a.modifier == b.modifier
            && a.skip == b.skip
            && a.hyperlink == b.hyperlink
    }

    fn covered_by_previous_wide_cell(cells: &[CellData], width: usize, idx: usize) -> bool {
        if idx == 0 || idx.is_multiple_of(width) {
            return false;
        }
        frame_cell_display_width(&cells[idx - 1]) > 1
    }

    fn frame_cell_display_width(cell: &CellData) -> usize {
        if is_halfwidth_katakana_voiced_grapheme(&cell.symbol) {
            return 2;
        }
        cell.symbol.width()
    }

    fn is_halfwidth_katakana_voiced_grapheme(symbol: &str) -> bool {
        let mut chars = symbol.chars();
        let Some(base) = chars.next() else {
            return false;
        };
        let Some(mark) = chars.next() else {
            return false;
        };
        chars.next().is_none()
            && ('\u{ff66}'..='\u{ff9d}').contains(&base)
            && matches!(mark, '\u{ff9e}' | '\u{ff9f}')
    }

    #[test]
    fn foreground_client_applies_client_keybindings() {
        let mut server = test_headless_server();
        let local_config: crate::config::Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_tab = "prefix+t"
"#,
        )
        .unwrap();
        let local_keybindings = local_config.live_keybinds().unwrap();
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, _control_b, _render_b) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_keybindings)),
            direct_attach_requested: false,
            direct_graphics: false,
            writer: writer_a,
        }));
        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('a')
        );
        assert!(server
            .app
            .state
            .keybinds
            .new_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+t"));

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer: writer_b,
        }));
        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('b')
        );
        assert!(server
            .app
            .state
            .keybinds
            .new_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+alt+c"));
    }

    #[test]
    fn local_keybinding_client_hides_server_keybinding_warnings() {
        let mut server = test_headless_server();
        let diagnostics = vec![
            "unsafe direct keybinding: keys.close_pane = \"x\" would intercept typing".to_owned(),
            "theme warning".to_owned(),
        ];
        let (full, without_keybindings) = server_config_diagnostic_summaries(&diagnostics);
        server.server_config_diagnostic = full.clone();
        server.server_config_diagnostic_without_keybindings = without_keybindings.clone();
        server.app.state.config_diagnostic = full;
        let local_keybindings = crate::config::Config::default().live_keybinds().unwrap();
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, _control_b, _render_b) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_keybindings)),
            direct_attach_requested: false,
            direct_graphics: false,
            writer: writer_a,
        }));
        assert_eq!(server.app.state.config_diagnostic, without_keybindings);

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer: writer_b,
        }));
        assert_eq!(
            server.app.state.config_diagnostic,
            server.server_config_diagnostic
        );
    }

    #[test]
    fn local_keybinding_client_keeps_local_keybindings_after_settings_save() {
        let path = std::env::temp_dir().join(format!(
            "herdr-headless-settings-{}.toml",
            crate::config::test_unique_suffix()
        ));
        std::fs::write(&path, "onboarding = false\n").unwrap();
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut server = test_headless_server();
        let local_config: crate::config::Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_workspace = "prefix+n"
next_tab = ""
"#,
        )
        .unwrap();
        let local_keybindings = local_config.live_keybinds().unwrap();
        let (writer, _control, _render) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_keybindings)),
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));
        server.app.state.set_server_mode(crate::app::Mode::Settings);
        server.app.state.settings.section = crate::app::state::SettingsSection::Toast;
        server.app.state.settings.list.selected = 1;

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\r".to_vec(),
        }));

        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('a')
        );
        assert!(server
            .app
            .state
            .keybinds
            .new_workspace
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+n"));
        assert!(server.app.state.toast.is_none());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("delivery = \"herdr\""));

        drop(env);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_server_keybindings_apply_valid_subset_after_settings_save_without_caching_local_keybindings(
    ) {
        let path = std::env::temp_dir().join(format!(
            "herdr-headless-invalid-settings-{}.toml",
            crate::config::test_unique_suffix()
        ));
        std::fs::write(
            &path,
            "onboarding = false\n[keys]\nnew_workspace = \"x\"\n[ui.toast]\ndelivery = \"off\"\n",
        )
        .unwrap();
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut server = test_headless_server();
        let previous_server_config: crate::config::Config =
            toml::from_str("[keys]\nprefix = \"ctrl+c\"\nnew_workspace = \"prefix+m\"\n").unwrap();
        server.server_keybindings = previous_server_config.live_keybinds().unwrap();
        let local_config: crate::config::Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_workspace = "prefix+n"
next_tab = ""
"#,
        )
        .unwrap();
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, _control_b, _render_b) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_config.live_keybinds().unwrap())),
            direct_attach_requested: false,
            direct_graphics: false,
            writer: writer_a,
        }));
        server.app.state.set_server_mode(crate::app::Mode::Settings);
        server.app.state.settings.section = crate::app::state::SettingsSection::Toast;
        server.app.state.settings.list.selected = 1;

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\r".to_vec(),
        }));

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer: writer_b,
        }));
        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('b')
        );
        assert!(!server
            .app
            .state
            .keybinds
            .new_workspace
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+n"));
        assert!(server.app.state.keybinds.new_workspace.bindings.is_empty());

        drop(env);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn terminal_attach_rejects_missing_terminal_and_removes_client() {
        let mut server = test_headless_server();
        let (writer, control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            direct_graphics: false,
            writer,
        }));
        assert!(server.clients.contains_key(&7));

        assert!(
            !server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 7,
                terminal_id: "term_missing".to_owned(),
                takeover: false,
            })
        );
        assert!(!server.clients.contains_key(&7));
        let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
        assert_eq!(
            reason,
            Some("terminal attach failed: terminal term_missing not found".to_owned())
        );
    }

    fn with_terminal_session_test_server(
        test: impl FnOnce(&mut HeadlessServer, crate::terminal::TerminalId, String, String),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).expect("terminal id").clone();
        let terminal_id_string = terminal_id.to_string();
        let public_pane_id = format!("{}:p1", workspace.id);
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.terminal_runtimes.insert(
            terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );

        test(&mut server, terminal_id, terminal_id_string, public_pane_id);

        drop(server);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    fn connect_pending_terminal_client(server: &mut HeadlessServer, client_id: u64) {
        let _control_rx = connect_pending_terminal_client_with_control_rx(server, client_id);
    }

    fn connect_pending_terminal_client_with_control_rx(
        server: &mut HeadlessServer,
        client_id: u64,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (writer, control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id,
            cols: 100,
            rows: 30,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            direct_graphics: false,
            writer,
        }));
        control_rx
    }

    #[cfg(unix)]
    fn test_remote_control_context(
        terminal_id: &str,
        workspace_id: &str,
        pane_id: &str,
    ) -> api::schema::RemoteControlContext {
        api::schema::RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: workspace_id.into(),
            tab_id: format!("{workspace_id}:t1"),
            pane_id: pane_id.into(),
            terminal_id: terminal_id.into(),
            cwd: "/work".into(),
            foreground_cwd: "/work".into(),
            tty: "/dev/pts/test".into(),
            foreground_process: api::schema::RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into()],
                cwd: "/work".into(),
            },
            detected_agent: "claude".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 1,
            revision: 1,
            context_epoch: 1,
        }
    }

    #[cfg(unix)]
    struct MutableSequencedContextProvider {
        contexts: std::sync::Mutex<std::collections::VecDeque<api::schema::RemoteControlContext>>,
    }

    #[cfg(unix)]
    impl crate::server::remote_control::RemoteControlContextProvider
        for MutableSequencedContextProvider
    {
        fn fresh_remote_control_context(
            &self,
            _agent_ref: &api::schema::AgentRef,
        ) -> Result<api::schema::RemoteControlContext, api::schema::ErrorBody> {
            self.contexts
                .lock()
                .expect("context provider lock")
                .pop_front()
                .ok_or_else(|| api::schema::ErrorBody {
                    code: "test_context_exhausted".into(),
                    message: "test context provider exhausted".into(),
                })
        }
    }

    #[cfg(unix)]
    fn install_controlled_test_client(
        server: &mut HeadlessServer,
        client_id: u64,
    ) -> (
        String,
        std::sync::mpsc::Receiver<Vec<u8>>,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        let workspace = crate::workspace::Workspace::test_new("controlled-test");
        let (runtime, input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 4,
            );
        let (terminal_id_string, control_rx) =
            install_controlled_runtime_client(server, client_id, workspace, runtime);
        (terminal_id_string, control_rx, input_rx)
    }

    #[cfg(unix)]
    fn install_controlled_runtime_client(
        server: &mut HeadlessServer,
        client_id: u64,
        workspace: crate::workspace::Workspace,
        runtime: crate::terminal::TerminalRuntime,
    ) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .expect("focused terminal")
            .clone();
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.agent_host_name = "buildbox".into();
        server.app.state.ensure_test_terminals();
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        let terminal_id_string = terminal_id.to_string();
        let workspace_id = server.app.state.workspaces[0].id.clone();
        let pane_id_string = format!(
            "{workspace_id}:p{}",
            server.app.state.workspaces[0]
                .public_pane_number(pane_id)
                .expect("public pane number")
        );
        let context =
            test_remote_control_context(&terminal_id_string, &workspace_id, &pane_id_string);
        let lease = crate::server::remote_control::RemoteControlLease::new(
            api::schema::AgentRef::new("buildbox", &pane_id_string).expect("agent ref"),
            context,
        );
        let (writer, control_rx, _render_rx) = test_client_writer();
        let mut client = test_app_client(Some(true), client_id);
        client.writer = Some(writer);
        client.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: terminal_id_string.clone(),
            control: Some(Box::new(lease)),
        };
        server.clients.insert(client_id, client);
        server
            .terminal_attach_owners
            .insert(terminal_id_string.clone(), client_id);
        assert!(server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("controlled runtime")
            .acquire_remote_owner(client_id));
        (terminal_id_string, control_rx)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn proxy_panes_do_not_consume_live_handoff_fd_budget() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("handoff-proxy");
        let local_terminal = workspace
            .terminal_id(workspace.tabs[0].root_pane)
            .expect("local terminal")
            .clone();
        let proxy_pane = crate::layout::PaneId::alloc();
        let proxy_terminal = crate::terminal::TerminalId::alloc();
        let (proxy_runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            proxy_pane,
            24,
            80,
            0,
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        let events = workspace.tabs[0].events.clone();
        workspace.create_tab_from_existing_pane(
            crate::workspace::MovedPane {
                pane_id: proxy_pane,
                pane_state: crate::pane::PaneState::new(proxy_terminal.clone()),
            },
            Some("remote-proxy".to_owned()),
            events,
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(crate::render_signal::RenderSignal::default()),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server
            .app
            .terminal_runtimes
            .insert(proxy_terminal.clone(), proxy_runtime);

        let panes = server.collect_handoff_panes();
        assert!(panes.contains_key(&local_terminal));
        assert!(!panes.contains_key(&proxy_terminal));

        server
            .app
            .terminal_runtimes
            .remove(&proxy_terminal)
            .expect("proxy runtime")
            .shutdown();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scheduled_tasks_do_not_recompute_attached_control_context() {
        let mut server = test_headless_server();
        let (terminal_id, _control_rx, _input_rx) = install_controlled_test_client(&mut server, 7);
        let real_terminal_id = server
            .terminal_id_by_string(&terminal_id)
            .expect("controlled terminal");
        server
            .app
            .terminal_runtimes
            .remove(&real_terminal_id)
            .expect("controlled runtime")
            .shutdown();

        server.handle_scheduled_tasks_headless(Instant::now(), false);

        assert!(
            server.clients.contains_key(&7),
            "scheduled work must not tear down a live control lease through a context mirror"
        );
    }

    #[cfg(target_os = "linux")]
    fn install_controlled_live_test_client(
        server: &mut HeadlessServer,
        client_id: u64,
    ) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
        let workspace = crate::workspace::Workspace::test_new("controlled-live-test");
        let pane_id = workspace.tabs[0].root_pane;
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane_id,
            24,
            80,
            std::env::temp_dir(),
            &[
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "stty -echo -icanon min 1 time 0; printf __herdr_control_ready__; exec sleep 30"
                    .to_owned(),
            ],
            &crate::pane::PaneLaunchEnv::default(),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            server.app.event_tx.clone(),
            server.app.render_notify.clone(),
            server.app.render_dirty.clone(),
        )
        .expect("spawn live controlled test runtime");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !runtime.visible_text().contains("__herdr_control_ready__") {
            assert!(
                std::time::Instant::now() < deadline,
                "live controlled test runtime did not become ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        install_controlled_runtime_client(server, client_id, workspace, runtime)
    }

    #[cfg(unix)]
    struct LocalSocketControlStream(crate::ipc::LocalStream);

    #[cfg(unix)]
    impl std::io::Read for LocalSocketControlStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            std::io::Read::read(&mut self.0, buffer)
        }
    }

    #[cfg(unix)]
    impl crate::remote::TimedRead for LocalSocketControlStream {
        fn read_with_timeout(
            &mut self,
            buffer: &mut [u8],
            _timeout: std::time::Duration,
        ) -> io::Result<usize> {
            std::io::Read::read(&mut self.0, buffer)
        }
    }

    #[cfg(unix)]
    impl std::io::Write for LocalSocketControlStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            std::io::Write::write(&mut self.0, buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            std::io::Write::flush(&mut self.0)
        }
    }

    #[cfg(unix)]
    impl crate::remote::ControlStream for LocalSocketControlStream {
        fn split(
            self: Box<Self>,
        ) -> (
            Box<dyn crate::remote::ControlReadHalf>,
            Box<dyn std::io::Write + Send>,
        ) {
            use interprocess::local_socket::traits::Stream as _;
            let (reader, writer) = self.0.split();
            (
                Box::new(LocalSocketControlReader(reader)),
                Box::new(LocalSocketControlWriter(writer)),
            )
        }
    }

    #[cfg(unix)]
    struct LocalSocketControlReader(interprocess::local_socket::RecvHalf);

    #[cfg(unix)]
    impl std::io::Read for LocalSocketControlReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            std::io::Read::read(&mut self.0, buffer)
        }
    }

    #[cfg(unix)]
    impl crate::remote::TimedRead for LocalSocketControlReader {
        fn read_with_timeout(
            &mut self,
            buffer: &mut [u8],
            _timeout: std::time::Duration,
        ) -> io::Result<usize> {
            std::io::Read::read(&mut self.0, buffer)
        }
    }

    #[cfg(unix)]
    impl crate::remote::ControlReadHalf for LocalSocketControlReader {}

    #[cfg(unix)]
    struct LocalSocketControlWriter(interprocess::local_socket::SendHalf);

    #[cfg(unix)]
    impl std::io::Write for LocalSocketControlWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            std::io::Write::write(&mut self.0, buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            std::io::Write::flush(&mut self.0)
        }
    }

    #[cfg(unix)]
    struct LocalSocketControlRunner {
        socket_path: PathBuf,
    }

    #[cfg(unix)]
    impl crate::remote::SshRunner for LocalSocketControlRunner {
        fn connect(
            &self,
            _host: &crate::config::FleetHostConfig,
        ) -> io::Result<Box<dyn crate::remote::ControlStream>> {
            Ok(Box::new(LocalSocketControlStream(
                crate::ipc::connect_local_stream(&self.socket_path)?,
            )))
        }
    }

    #[cfg(unix)]
    fn guarded_control_handshake_test_server() -> (
        HeadlessServer,
        api::schema::AgentRef,
        api::schema::RemoteControlContext,
    ) {
        guarded_control_handshake_test_server_with_command("exec sleep 30")
    }

    #[cfg(unix)]
    fn guarded_control_handshake_test_server_with_command(
        command: &str,
    ) -> (
        HeadlessServer,
        api::schema::AgentRef,
        api::schema::RemoteControlContext,
    ) {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("guarded-control-handshake");
        let pane_id = workspace.tabs[0].root_pane;
        let workspace_id = workspace.id.clone();
        let pane_id_string = format!("{workspace_id}:p1");
        let agent_ref = api::schema::AgentRef::new("buildbox", &pane_id_string)
            .expect("valid guarded handshake agent reference");
        let runtime = crate::terminal::TerminalRuntime::spawn_argv_command(
            pane_id,
            24,
            80,
            std::env::temp_dir(),
            &["/bin/sh".to_owned(), "-c".to_owned(), command.to_owned()],
            &crate::pane::PaneLaunchEnv::from_extra(vec![(
                "HERDR_AGENT".to_owned(),
                "claude".to_owned(),
            )]),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            server.app.event_tx.clone(),
            server.app.render_notify.clone(),
            server.app.render_dirty.clone(),
        )
        .expect("spawn guarded handshake runtime");

        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.agent_host_name = "buildbox".to_owned();
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("guarded handshake terminal")
            .clone();
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        let terminal = server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("guarded handshake terminal state");
        let now = Instant::now();
        terminal.begin_managed_agent(
            "claude".to_owned(),
            crate::detect::Agent::Claude,
            now,
            Duration::ZERO,
            Duration::from_secs(30),
        );
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(1), false));
        terminal.last_agent_state_change_seq = Some(1);
        assert!(terminal.managed_agent_control_ready());

        let deadline = Instant::now() + Duration::from_secs(2);
        let context = loop {
            match server.app.remote_control_context(&agent_ref) {
                Ok(context) => break context,
                Err(_error) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    let terminal_id = server.app.state.workspaces[0]
                        .terminal_id(pane_id)
                        .expect("guarded handshake terminal")
                        .clone();
                    let runtime = server
                        .app
                        .terminal_runtimes
                        .get(&terminal_id)
                        .expect("guarded handshake runtime");
                    let child_pid = runtime.child_pid().unwrap_or_default();
                    panic!(
                        "guarded handshake runtime did not expose a valid context: {}: {}; pid={child_pid}, job={:?}, hint={:?}",
                        error.code,
                        error.message,
                        crate::detect::foreground_job(child_pid),
                        crate::platform::process_agent_hint(child_pid),
                    );
                }
            }
        };
        (server, agent_ref, context)
    }

    #[cfg(unix)]
    fn run_real_guarded_control_handshake(
        server: &mut HeadlessServer,
        agent_ref: &api::schema::AgentRef,
        expected_context: Option<api::schema::RemoteControlContext>,
        version: u32,
    ) -> crate::app::remote_focus::RemoteFocusTransition {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: agent_ref.host.clone(),
                target: "local-test-control-socket".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut transport = crate::remote::SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(LocalSocketControlRunner {
                socket_path: server.client_socket_path.clone(),
            }),
        );
        transport.observe_fleet_snapshot(&crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: agent_ref.host.clone(),
                target: "local-test-control-socket".to_owned(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: Some(agent_ref.host.clone()),
                entries: Vec::new(),
            }],
            ..Default::default()
        });
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
        let channels = crate::pane::RemoteProxyChannels {
            outbound_rx,
            detach_tx: outbound_tx,
            resize_slot: Arc::new(std::sync::Mutex::new((24, 80, 0, 0))),
            input_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            operation_state: crate::remote::RemoteFocusOperationState::new(),
        };
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start_with_expected_context_and_version_for_test(
                "guarded-control-handshake",
                agent_ref,
                expected_context,
                version,
                channels,
                event_tx,
            )
            .expect("control client thread starts");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            server
                .accept_client_connections()
                .expect("accept control client connection");
            while let Ok(event) = server.server_event_rx.try_recv() {
                match event {
                    ServerEvent::ClientConnected { .. }
                    | ServerEvent::ClientControlTerminal { .. } => {
                        server.handle_server_event(event);
                    }
                    ServerEvent::ClientDisconnected { .. }
                    | ServerEvent::ClientWriterDrained { .. } => {
                        server.handle_server_event(event);
                    }
                    other => panic!("unexpected server event in control handshake: {other:?}"),
                }
            }
            match event_rx.try_recv() {
                Ok(AppEvent::RemoteFocusTransition { transition, .. }) => {
                    let transition = *transition;
                    if matches!(
                        transition,
                        crate::app::remote_focus::RemoteFocusTransition::Active(_)
                    ) {
                        let client_id = server
                            .clients
                            .keys()
                            .next()
                            .copied()
                            .expect("active control client remains registered");
                        server.remove_client(client_id);
                        assert!(!server.clients.contains_key(&client_id));
                    }
                    return transition;
                }
                Ok(other) => panic!("unexpected client event in control handshake: {other:?}"),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("control handshake did not complete: {error}"),
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC6: the real client/server control handshake returns ControlReady for a matching context.
    async fn real_control_handshake_returns_control_ready_to_matching_client() {
        let (mut server, agent_ref, expected_context) = guarded_control_handshake_test_server();
        let transition =
            run_real_guarded_control_handshake(&mut server, &agent_ref, None, PROTOCOL_VERSION);
        match transition {
            crate::app::remote_focus::RemoteFocusTransition::Active(context) => {
                assert_eq!(context.host, "buildbox");
                assert_eq!(context.terminal_id, expected_context.terminal_id);
            }
            other => panic!("expected client-classified ControlReady, got {other:?}"),
        }
        shutdown_test_runtimes(&mut server);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC6: the real guarded server emits ControlError for a context mismatch and the client classifies it.
    async fn real_control_handshake_classifies_context_mismatch_as_typed_error() {
        let (mut server, agent_ref, mut expected) = guarded_control_handshake_test_server();
        expected.revision = expected.revision.saturating_add(1);
        let transition = run_real_guarded_control_handshake(
            &mut server,
            &agent_ref,
            Some(expected),
            PROTOCOL_VERSION,
        );
        match transition {
            crate::app::remote_focus::RemoteFocusTransition::Failed(error) => {
                assert_eq!(error.code, "refused_for_safety");
                assert!(error.message.contains("context"));
            }
            other => panic!("expected client-classified ControlError, got {other:?}"),
        }
        assert!(server.clients.is_empty());
        shutdown_test_runtimes(&mut server);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC6: a real client/server protocol version mismatch is classified as version_skew.
    async fn real_control_handshake_classifies_version_mismatch_as_version_skew() {
        let (mut server, agent_ref, _) = guarded_control_handshake_test_server();
        let transition = run_real_guarded_control_handshake(
            &mut server,
            &agent_ref,
            None,
            PROTOCOL_VERSION.saturating_sub(1),
        );
        match transition {
            crate::app::remote_focus::RemoteFocusTransition::Failed(error) => {
                assert_eq!(error.code, "version_skew");
            }
            other => panic!("expected client-classified version_skew, got {other:?}"),
        }
        assert!(server.clients.is_empty());
        shutdown_test_runtimes(&mut server);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC6: a real client/server handshake with a higher client version is classified as version_skew.
    async fn real_control_handshake_classifies_higher_version_as_version_skew() {
        let (mut server, agent_ref, _) = guarded_control_handshake_test_server();
        let transition =
            run_real_guarded_control_handshake(&mut server, &agent_ref, None, PROTOCOL_VERSION + 1);
        match transition {
            crate::app::remote_focus::RemoteFocusTransition::Failed(error) => {
                assert_eq!(error.code, "version_skew");
            }
            other => panic!("expected client-classified version_skew, got {other:?}"),
        }
        assert!(server.clients.is_empty());
        shutdown_test_runtimes(&mut server);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    // The whole proxy loop over the real wire: one local proxy pane shows the
    // remote terminal, input stays gated until ControlReady and the first
    // complete frame, typed input reaches the remote PTY, the answer comes
    // back as frames, and closing the pane releases the remote lease.
    async fn real_control_session_streams_to_proxy_pane_and_back() {
        let (mut remote, agent_ref, remote_context) =
            guarded_control_handshake_test_server_with_command(
                "while IFS= read -r line; do printf 'echo:%s\\n' \"$line\"; done",
            );
        let remote_terminal_id = remote.app.state.workspaces[0]
            .terminal_id(remote.app.state.workspaces[0].tabs[0].root_pane)
            .expect("remote terminal")
            .clone();

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut client = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        client.state.workspaces = vec![crate::workspace::Workspace::test_new("client")];
        client.state.ensure_test_terminals();
        client.state.active = Some(0);
        client.state.selected = 0;
        client.state.set_server_mode(crate::app::Mode::Terminal);
        client.state.agent_host_name = "client".to_owned();
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "local-test-control-socket".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        client.remote_focus_transport =
            Box::new(crate::remote::SshRemoteFocusTransport::with_runner(
                &fleet,
                Arc::new(LocalSocketControlRunner {
                    socket_path: remote.client_socket_path.clone(),
                }),
            ));
        client
            .remote_focus_transport
            .observe_fleet_snapshot(&crate::fleet::Snapshot {
                hosts: vec![crate::fleet::HostSnapshot {
                    name: "buildbox".into(),
                    target: "local-test-control-socket".to_owned(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::HostState::Reachable,
                    version: None,
                    protocol: None,
                    error: None,
                    remote_identity: Some(remote_context.host),
                    entries: Vec::new(),
                }],
                ..Default::default()
            });
        let started = client
            .start_remote_focus_operation(agent_ref.clone())
            .expect("remote focus operation starts");
        let operation_id = started.operation_id.clone();

        // Pumps control-plane traffic only: connections, server events, and
        // client events. Terminal frames leave the server exclusively through
        // `render_and_stream`, so driving without it guarantees no frame can
        // reach the proxy pane while this pump is in use.
        fn drive_control(remote: &mut HeadlessServer, client: &mut crate::app::App) {
            remote
                .accept_client_connections()
                .expect("accept control connection");
            while let Ok(event) = remote.server_event_rx.try_recv() {
                remote.handle_server_event(event);
            }
            while let Ok(event) = client.event_rx.try_recv() {
                client.handle_internal_event_with_render_impact(event);
            }
        }

        fn drive(remote: &mut HeadlessServer, client: &mut crate::app::App) {
            drive_control(remote, client);
            remote.render_and_stream();
            while let Ok(event) = client.event_rx.try_recv() {
                client.handle_internal_event_with_render_impact(event);
            }
        }

        fn drive_until(
            remote: &mut HeadlessServer,
            client: &mut crate::app::App,
            what: &str,
            pump: fn(&mut HeadlessServer, &mut crate::app::App),
            condition: impl Fn(&mut HeadlessServer, &mut crate::app::App) -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !condition(remote, client) {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                pump(remote, client);
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        // ControlReady activates the operation and stamps the identity line.
        // Drive the control plane without rendering so the first frame stays
        // unsent and the input gate stays shut until the test opens it below.
        drive_until(
            &mut remote,
            &mut client,
            "control ready",
            drive_control,
            |_remote, client| {
                client
                    .remote_focus_status(&operation_id)
                    .is_ok_and(|status| status.state == api::schema::RemoteFocusState::Active)
            },
        );
        let (_proxy_pane, proxy_terminal_id) = client
            .remote_focus_operations
            .proxy_location(&operation_id)
            .expect("proxy pane bound");
        {
            let terminal = client
                .state
                .terminals
                .get(&proxy_terminal_id)
                .expect("proxy terminal");
            let label = terminal.manual_label.as_deref().expect("identity line");
            assert!(label.starts_with("buildbox::"), "identity line: {label}");
            // The line must carry the server's real tty path, whatever the
            // platform names it ("/dev/pts/N" on Linux, "/dev/ttysNNN" on
            // macOS), so compare against the live remote runtime instead of a
            // hardcoded prefix.
            let remote_tty = remote
                .app
                .terminal_runtimes
                .get(&remote_terminal_id)
                .and_then(|runtime| runtime.tty_name().map(Path::to_path_buf))
                .expect("remote pane tty");
            assert!(
                label.contains(remote_tty.to_str().expect("utf-8 tty path")),
                "identity line: {label}"
            );
        }
        // No complete frame yet (nothing was ever rendered to this client):
        // input stays refused and is not buffered.
        assert!(
            client
                .terminal_runtimes
                .get(&proxy_terminal_id)
                .expect("proxy runtime")
                .try_send_bytes(Bytes::from_static(b"too early\n"))
                .is_err(),
            "input before the first complete frame is refused"
        );

        // Rendering starts now: the first complete remote frame opens the gate.
        drive_until(
            &mut remote,
            &mut client,
            "first complete frame",
            drive,
            |_remote, client| {
                client
                    .remote_focus_operations
                    .input_gate_open(&operation_id)
            },
        );

        // Typed input crosses the wire, is written to the remote PTY by the
        // guarded batch path, and the remote output returns as frames.
        client
            .terminal_runtimes
            .get(&proxy_terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(Bytes::from_static(b"hello remote\n"))
            .expect("input enabled after ControlReady and first frame");
        drive_until(
            &mut remote,
            &mut client,
            "remote echo",
            drive,
            |_remote, client| {
                client
                    .terminal_runtimes
                    .get(&proxy_terminal_id)
                    .is_some_and(|runtime| runtime.visible_text().contains("echo:hello remote"))
            },
        );

        // Closing the proxy pane releases the remote lease and the operation
        // ends Closed, not Failed.
        let (proxy_pane_id, _) = client
            .remote_focus_operations
            .proxy_location(&operation_id)
            .expect("proxy pane bound");
        let (proxy_ws_idx, _) = client.find_pane(proxy_pane_id).expect("proxy pane placed");
        assert!(client.state.workspaces[proxy_ws_idx].is_fleet);
        // The proxy is the fleet space's only pane, so closing it closes the space.
        if client.state.workspaces[proxy_ws_idx].remove_pane(proxy_pane_id) {
            client.state.workspaces.remove(proxy_ws_idx);
        }
        client
            .state
            .remove_unattached_terminal_ids([proxy_terminal_id.clone()]);
        client.shutdown_detached_terminal_runtimes();
        assert_eq!(
            client
                .remote_focus_status(&operation_id)
                .expect("status")
                .state,
            api::schema::RemoteFocusState::Closed
        );
        drive_until(
            &mut remote,
            &mut client,
            "lease release",
            drive,
            |remote, _client| remote.clients.is_empty(),
        );
        assert!(
            remote
                .app
                .terminal_runtimes
                .get(&remote_terminal_id)
                .expect("remote runtime")
                .acquire_remote_owner(99),
            "the remote terminal accepts a new owner after the lease released"
        );

        shutdown_test_runtimes(&mut remote);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC2: a context change between control batches prevents the second PTY write.
    async fn forward_control_bytes_refreshes_context_for_each_batch() {
        let mut server = test_headless_server();
        let (terminal_id, control_rx, mut input_rx) =
            install_controlled_test_client(&mut server, 7);
        let expected = match &server.clients[&7].mode {
            ClientConnectionMode::TerminalAttach {
                control: Some(lease),
                ..
            } => lease.context.clone(),
            _ => panic!("controlled test client has no lease"),
        };
        let mut changed = expected.clone();
        changed.revision += 1;
        let provider = MutableSequencedContextProvider {
            contexts: std::sync::Mutex::new(
                [expected.clone(), changed]
                    .into_iter()
                    .collect::<std::collections::VecDeque<_>>(),
            ),
        };

        assert!(server.forward_control_bytes_with_provider_for_test(
            7,
            b"first".to_vec(),
            &provider,
        ));
        assert_eq!(
            input_rx.try_recv().expect("first batch written"),
            Bytes::from("first")
        );
        assert!(!server.forward_control_bytes_with_provider_for_test(
            7,
            b"second".to_vec(),
            &provider,
        ));
        assert!(
            input_rx.try_recv().is_err(),
            "second batch must not be written"
        );
        assert!(!server.clients.contains_key(&7));
        assert!(matches!(
            read_server_message(control_rx.recv().expect("typed control error")),
            ServerMessage::ControlError { code, .. } if code == "refused_for_safety"
        ));
        assert!(provider
            .contexts
            .lock()
            .expect("context provider lock")
            .is_empty());
        assert!(server
            .app
            .terminal_runtimes
            .get(
                &server
                    .terminal_id_by_string(&terminal_id)
                    .expect("terminal")
            )
            .expect("runtime")
            .acquire_remote_owner(99));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC3: a partial controlled PTY write ends the lease without retrying the remainder.
    async fn partial_controlled_write_ends_server_lease() {
        let mut server = test_headless_server();
        let (terminal_id, control_rx, _input_rx) = install_controlled_test_client(&mut server, 7);
        let real_terminal_id = server
            .terminal_id_by_string(&terminal_id)
            .expect("terminal");

        assert!(!server.finish_controlled_write(
            7,
            &real_terminal_id,
            true,
            crate::pty::actor::ControlledWriteResult::DeliveryUnknown { written: 17 },
        ));
        assert!(!server.clients.contains_key(&7));
        assert!(!server.terminal_attach_owners.contains_key(&terminal_id));
        assert!(matches!(
            read_server_message(control_rx.recv().expect("typed control error")),
            ServerMessage::ControlError { code, message }
                if code == "connection_lost" && message.contains("17 bytes")
        ));
        assert!(server
            .app
            .terminal_runtimes
            .get(
                &server
                    .terminal_id_by_string(&terminal_id)
                    .expect("terminal")
            )
            .expect("runtime")
            .acquire_remote_owner(99));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    // AC3: a real partial controlled PTY result ends the server lease without retrying.
    async fn partial_real_controlled_write_ends_server_lease_without_retry() {
        let mut server = test_headless_server();
        let (terminal_id, control_rx) = install_controlled_live_test_client(&mut server, 7);
        let real_terminal_id = server
            .terminal_id_by_string(&terminal_id)
            .expect("terminal")
            .clone();
        let expected = match &server.clients[&7].mode {
            ClientConnectionMode::TerminalAttach {
                control: Some(lease),
                ..
            } => lease.context.clone(),
            _ => panic!("controlled test client has no lease"),
        };
        let provider = MutableSequencedContextProvider {
            contexts: std::sync::Mutex::new(
                [expected]
                    .into_iter()
                    .collect::<std::collections::VecDeque<_>>(),
            ),
        };
        let payload = vec![b'x'; 1024 * 1024];

        assert!(!server.forward_control_bytes_with_provider_for_test(
            7,
            payload.clone(),
            &provider,
        ));
        let ServerMessage::ControlError { code, message } =
            read_server_message(control_rx.recv().expect("partial-write control error"))
        else {
            panic!("expected partial-write control error");
        };
        assert_eq!(code, "connection_lost");
        let written = message
            .strip_prefix("controlled PTY delivery became unknown after ")
            .and_then(|message| message.strip_suffix(" bytes; no retry"))
            .and_then(|written| written.parse::<usize>().ok())
            .expect("partial byte count in control error");
        assert!(written > 0 && written < payload.len(), "written={written}");
        assert!(!server.clients.contains_key(&7));
        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 7,
            data: b"retry-must-not-arrive".to_vec(),
        }));
        assert!(server
            .app
            .terminal_runtimes
            .get(&real_terminal_id)
            .expect("live runtime")
            .acquire_remote_owner(99));
        shutdown_test_runtimes(&mut server);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    // AC3: a real EAGAIN controlled PTY result ends the server lease without retrying.
    async fn eagain_real_controlled_write_ends_server_lease_without_retry() {
        let mut server = test_headless_server();
        let (terminal_id, control_rx) = install_controlled_live_test_client(&mut server, 7);
        let real_terminal_id = server
            .terminal_id_by_string(&terminal_id)
            .expect("terminal")
            .clone();
        let runtime = server
            .app
            .terminal_runtimes
            .get(&real_terminal_id)
            .expect("live runtime");
        // Fill the kernel input queue one byte at a time so the server payload
        // below observes a stable, real zero-byte EAGAIN result.
        let mut reached_eagain = false;
        for _ in 0..131_072 {
            match runtime.try_send_controlled_bytes(7, b"f") {
                crate::pty::actor::ControlledWriteResult::Written => {}
                crate::pty::actor::ControlledWriteResult::DeliveryUnknown { written: 0 } => {
                    reached_eagain = true;
                    break;
                }
                crate::pty::actor::ControlledWriteResult::DeliveryUnknown { written } => {
                    panic!("one-byte EAGAIN setup partially wrote {written} bytes")
                }
                crate::pty::actor::ControlledWriteResult::Refused => {
                    panic!("controlled owner unexpectedly refused during EAGAIN setup")
                }
            }
        }
        assert!(reached_eagain, "real PTY input buffer did not reach EAGAIN");

        let expected = match &server.clients[&7].mode {
            ClientConnectionMode::TerminalAttach {
                control: Some(lease),
                ..
            } => lease.context.clone(),
            _ => panic!("controlled test client has no lease"),
        };
        let provider = MutableSequencedContextProvider {
            contexts: std::sync::Mutex::new(
                [expected]
                    .into_iter()
                    .collect::<std::collections::VecDeque<_>>(),
            ),
        };
        assert!(!server.forward_control_bytes_with_provider_for_test(
            7,
            b"eagain-payload-must-not-arrive".to_vec(),
            &provider,
        ));
        let ServerMessage::ControlError { code, message } =
            read_server_message(control_rx.recv().expect("EAGAIN control error"))
        else {
            panic!("expected EAGAIN control error");
        };
        assert_eq!(code, "connection_lost");
        assert!(message.contains("after 0 bytes; no retry"), "{message}");
        assert!(!server.clients.contains_key(&7));
        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 7,
            data: b"retry-must-not-arrive".to_vec(),
        }));
        assert!(server
            .app
            .terminal_runtimes
            .get(&real_terminal_id)
            .expect("live runtime")
            .acquire_remote_owner(99));
        shutdown_test_runtimes(&mut server);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC3: an EAGAIN result is delivery-unknown and ends the lease automatically.
    async fn eagain_controlled_write_ends_server_lease() {
        let mut server = test_headless_server();
        let (terminal_id, control_rx, _input_rx) = install_controlled_test_client(&mut server, 7);
        let real_terminal_id = server
            .terminal_id_by_string(&terminal_id)
            .expect("terminal");

        assert!(!server.finish_controlled_write(
            7,
            &real_terminal_id,
            true,
            crate::pty::actor::ControlledWriteResult::DeliveryUnknown { written: 0 },
        ));
        assert!(!server.clients.contains_key(&7));
        assert!(matches!(
            read_server_message(control_rx.recv().expect("typed control error")),
            ServerMessage::ControlError { code, message }
                if code == "connection_lost" && message.contains("0 bytes")
        ));
        assert!(server
            .app
            .terminal_runtimes
            .get(
                &server
                    .terminal_id_by_string(&terminal_id)
                    .expect("terminal")
            )
            .expect("runtime")
            .acquire_remote_owner(99));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC6: the server emits a typed ControlError instead of an untyped shutdown.
    async fn server_emits_typed_control_error_for_remote_rejection() {
        let mut server = test_headless_server();
        let (writer, control_rx, _render_rx) = test_client_writer();
        let mut client = test_app_client(Some(true), 7);
        client.writer = Some(writer);
        server.clients.insert(7, client);

        assert!(!server.reject_remote_control(
            7,
            api::schema::ErrorBody {
                code: "refused_for_safety".into(),
                message: "test refusal".into(),
            },
        ));
        assert!(matches!(
            read_server_message(control_rx.recv().expect("typed control error")),
            ServerMessage::ControlError { code, message }
                if code == "refused_for_safety" && message == "test refusal"
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC1: a clipboard-image paste uses the human-input lease termination path before pane write.
    async fn clipboard_image_paste_ends_human_control_before_forwarding() {
        let mut server = test_headless_server();
        let (terminal_id, control_rx, mut input_rx) =
            install_controlled_test_client(&mut server, 7);
        let mut app_client = test_app_client(Some(true), 1);
        app_client.pending_terminal_attach = false;
        server.clients.insert(1, app_client);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server
            .app
            .terminal_runtimes
            .get(
                &server
                    .terminal_id_by_string(&terminal_id)
                    .expect("terminal")
            )
            .expect("runtime")
            .try_send_paste("blocked-before-human-input".into())
            .is_err());
        assert!(
            server.handle_server_event(ServerEvent::ClientClipboardImage {
                client_id: 1,
                extension: "png".into(),
                data: b"not-a-real-image".to_vec(),
            })
        );
        assert!(!server.clients.contains_key(&7));
        let forwarded = input_rx.try_recv().expect("clipboard image path forwarded");
        let forwarded = String::from_utf8_lossy(&forwarded);
        assert!(forwarded.contains("herdr-clipboard-images"), "{forwarded}");
        assert!(!server.forward_control_bytes(7, b"controlled-after-clipboard".to_vec()));
        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 7,
            data: b"controlled-after-clipboard".to_vec(),
        }));
        assert!(
            input_rx.try_recv().is_err(),
            "controlled write must not retry"
        );
        assert!(server
            .app
            .terminal_runtimes
            .get(
                &server
                    .terminal_id_by_string(&terminal_id)
                    .expect("terminal")
            )
            .expect("runtime")
            .acquire_remote_owner(99));
        assert!(matches!(
            read_server_message(control_rx.recv().expect("clipboard control error")),
            ServerMessage::ControlError { code, message }
                if code == "already_controlled" && message.contains("human input")
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC1: a server-routed human keystroke ends the remote lease before a later controlled write.
    async fn human_keystroke_ends_server_remote_lease_before_following_controlled_write() {
        let mut server = test_headless_server();
        let (terminal_id, control_rx, mut input_rx) =
            install_controlled_test_client(&mut server, 7);
        let mut app_client = test_app_client(Some(true), 1);
        app_client.pending_terminal_attach = false;
        server.clients.insert(1, app_client);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('x'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));
        assert_eq!(
            input_rx.try_recv().expect("human key forwarded"),
            Bytes::from("x")
        );
        assert!(!server.clients.contains_key(&7));
        assert!(!server.forward_control_bytes(7, b"controlled-after-key".to_vec()));
        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 7,
            data: b"controlled-after-key".to_vec(),
        }));
        assert!(
            input_rx.try_recv().is_err(),
            "controlled write must not retry"
        );
        assert!(server
            .app
            .terminal_runtimes
            .get(
                &server
                    .terminal_id_by_string(&terminal_id)
                    .expect("terminal")
            )
            .expect("runtime")
            .acquire_remote_owner(99));
        assert!(matches!(
            read_server_message(control_rx.recv().expect("human-key control error")),
            ServerMessage::ControlError { code, message }
                if code == "already_controlled" && message.contains("human input")
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn unrelated_pane_api_mutation_preserves_remote_lease() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("remote-control-api");
        let controlled_pane = workspace.tabs[0].root_pane;
        let unrelated_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let workspace_id = workspace.id.clone();
        let controlled_terminal = workspace
            .terminal_id(controlled_pane)
            .expect("controlled terminal")
            .clone();
        let controlled_terminal_string = controlled_terminal.to_string();
        let unrelated_pane_id = format!(
            "{workspace_id}:p{}",
            workspace
                .public_pane_number(unrelated_pane)
                .expect("unrelated public pane")
        );
        let controlled_pane_id = format!(
            "{workspace_id}:p{}",
            workspace
                .public_pane_number(controlled_pane)
                .expect("controlled public pane")
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.terminal_runtimes.insert(
            controlled_terminal.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );

        let context = test_remote_control_context(
            &controlled_terminal_string,
            &workspace_id,
            &controlled_pane_id,
        );
        let lease = crate::server::remote_control::RemoteControlLease::new(
            api::schema::AgentRef::new("buildbox", &controlled_pane_id).expect("agent ref"),
            context,
        );
        let mut client = test_app_client(Some(true), 1);
        client.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: controlled_terminal_string.clone(),
            control: Some(Box::new(lease)),
        };
        server.clients.insert(7, client);
        server
            .terminal_attach_owners
            .insert(controlled_terminal_string.clone(), 7);
        assert!(server
            .app
            .terminal_runtimes
            .get(&controlled_terminal)
            .expect("controlled runtime")
            .acquire_remote_owner(7));

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "rename-unrelated".into(),
                    method: api::schema::Method::PaneRename(api::schema::PaneRenameParams {
                        pane_id: unrelated_pane_id,
                        label: Some("unrelated".into()),
                    }),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );
        assert!(response_rx.recv_timeout(Duration::from_millis(100)).is_ok());
        assert!(matches!(
            server.clients.get(&7).map(|client| &client.mode),
            Some(ClientConnectionMode::TerminalAttach {
                control: Some(_),
                ..
            })
        ));
        assert_eq!(
            server
                .terminal_attach_owners
                .get(&controlled_terminal_string),
            Some(&7)
        );
        assert!(!server
            .app
            .terminal_runtimes
            .get(&controlled_terminal)
            .expect("controlled runtime")
            .acquire_remote_owner(99));
    }

    #[test]
    fn explicit_agent_history_read_requires_idle_on_alternate_screen() {
        with_terminal_session_test_server(
            |server, terminal_id, _terminal_id_string, public_pane_id| {
                let terminal = server
                    .app
                    .state
                    .terminals
                    .get_mut(&terminal_id)
                    .expect("terminal");
                terminal.detected_agent = Some(crate::detect::Agent::Claude);
                terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Working);
                server.app.terminal_runtimes.insert(
                    terminal_id,
                    crate::terminal::TerminalRuntime::test_with_screen_bytes(
                        80,
                        24,
                        b"\x1b[?1049hworking",
                    ),
                );
                let request = api::schema::Request {
                    id: "read".into(),
                    method: api::schema::Method::AgentRead(api::schema::AgentReadParams {
                        target: public_pane_id.clone(),
                        source: api::schema::ReadSource::Recent,
                        lines: Some(200),
                        format: api::schema::ReadFormat::Text,
                        strip_ansi: true,
                    }),
                };

                assert_eq!(
                    server.agent_read_not_idle_error(&request),
                    Some(api::schema::ErrorBody {
                        code: "agent_not_idle".into(),
                        message: format!(
                            "cannot read 200 lines while {public_pane_id} is working: its alternate-screen history can only be captured by scrolling while idle. Wait and retry, or use --source visible"
                        ),
                    })
                );

                let mut default_request = request.clone();
                let api::schema::Method::AgentRead(params) = &mut default_request.method else {
                    unreachable!();
                };
                params.lines = None;
                assert_eq!(server.agent_read_not_idle_error(&default_request), None);

                let mut visible_request = request;
                let api::schema::Method::AgentRead(params) = &mut visible_request.method else {
                    unreachable!();
                };
                params.source = api::schema::ReadSource::Visible;
                assert_eq!(server.agent_read_not_idle_error(&visible_request), None);
            },
        );
    }

    #[test]
    fn terminal_observe_allows_multiple_clients_without_attach_ownership() {
        with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
            for client_id in [7, 8] {
                connect_pending_terminal_client(server, client_id);
                if client_id == 8 {
                    server.clients.get_mut(&client_id).unwrap().terminal_size = (120, 40);
                }
                assert!(
                    server.handle_server_event(ServerEvent::ClientObserveTerminal {
                        client_id,
                        target: terminal_id_string.clone(),
                    })
                );
            }

            assert!(server.terminal_attach_owners.is_empty());
            assert!(!server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));
            assert_eq!(
                server
                    .app
                    .terminal_runtimes
                    .get(&terminal_id)
                    .expect("runtime")
                    .current_size(),
                (30, 100)
            );
            assert_eq!(
                terminal_stream_client_ids(&server.clients, &terminal_id_string).len(),
                2
            );
        });
    }

    #[test]
    fn terminal_observe_and_attach_promote_remaining_client_input_source() {
        for observe in [true, false] {
            with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
                server.app.state.switch_ascii_input_source_in_prefix = true;
                connect_pending_terminal_client(server, 1);
                let (writer, control_rx, _render_rx) = test_client_writer();
                let mut context_owner = test_app_client(Some(true), 2);
                context_owner.writer = Some(writer);
                context_owner.sidebar_presentation.overlay.kind =
                    crate::app::state::ClientOverlay::ContextMenu;
                server.clients.insert(2, context_owner);
                server.foreground_client_id = Some(1);
                server.sync_foreground_client_state();

                let changed = if observe {
                    server.handle_server_event(ServerEvent::ClientObserveTerminal {
                        client_id: 1,
                        target: terminal_id_string,
                    })
                } else {
                    server.handle_server_event(ServerEvent::ClientAttachTerminal {
                        client_id: 1,
                        terminal_id: terminal_id_string,
                        takeover: false,
                    })
                };

                assert!(changed);
                assert_eq!(server.foreground_client_id, Some(2));
                assert!(matches!(
                    read_server_message(
                        control_rx
                            .recv_timeout(Duration::from_millis(100))
                            .expect("remaining client input-source state"),
                    ),
                    ServerMessage::PrefixInputSource { active: true }
                ));
            });
        }
    }

    #[test]
    fn terminal_observe_resolves_public_pane_id() {
        with_terminal_session_test_server(|server, terminal_id, _, public_pane_id| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 7,
                    target: public_pane_id,
                })
            );

            assert!(matches!(
                server.clients.get(&7).map(|client| &client.mode),
                Some(ClientConnectionMode::TerminalObserve { terminal_id: observed })
                    if observed == &terminal_id.to_string()
            ));
        });
    }

    #[test]
    fn terminal_control_resolves_public_pane_id_and_takes_ownership() {
        with_terminal_session_test_server(
            |server, terminal_id, terminal_id_string, public_pane_id| {
                connect_pending_terminal_client(server, 7);
                assert!(
                    server.handle_server_event(ServerEvent::ClientControlTerminal {
                        client_id: 7,
                        target: public_pane_id,
                        agent_ref: None,
                        expected_context: None,
                        takeover: false,
                    })
                );

                assert!(matches!(
                    server.clients.get(&7).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalAttach {
                        terminal_id: attached,
                        ..
                    })
                        if attached == &terminal_id_string
                ));
                assert_eq!(
                    server.terminal_attach_owners.get(&terminal_id_string),
                    Some(&7)
                );
                assert!(server
                    .app
                    .state
                    .direct_attach_resize_locks
                    .contains(&terminal_id));
            },
        );
    }

    #[test]
    fn terminal_control_rejects_attach_during_alt_screen_read() {
        with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
            let (respond_to, _response_rx) = std::sync::mpsc::channel();
            server.pending_alt_screen_reads.push(
                crate::server::alt_screen_read::PendingAltScreenRead::start(
                    terminal_id,
                    "read".into(),
                    respond_to,
                    "fallback".into(),
                    api::schema::PaneReadResult {
                        pane_id: "w1:p1".into(),
                        terminal_id: terminal_id_string.clone(),
                        workspace_id: "w1".into(),
                        tab_id: "w1:t1".into(),
                        agent_ref: None,
                        agent_session: None,
                        source: api::schema::ReadSource::Recent,
                        format: api::schema::ReadFormat::Text,
                        text: String::new(),
                        revision: 0,
                        truncated: false,
                        input_observation: None,
                    },
                    120,
                    false,
                    crate::terminal::ScreenSnapshot {
                        cols: 80,
                        rows: Vec::new(),
                    },
                    Instant::now(),
                ),
            );
            let control_rx = connect_pending_terminal_client_with_control_rx(server, 7);

            assert!(
                !server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    agent_ref: None,
                    expected_context: None,
                    takeover: false,
                })
            );
            assert!(!server.clients.contains_key(&7));
            assert!(!server
                .terminal_attach_owners
                .contains_key(&terminal_id_string));
            let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
            assert_eq!(
                reason,
                Some(format!(
                    "terminal attach failed: terminal {terminal_id_string} has a read in progress; retry"
                ))
            );
        });
    }

    #[test]
    fn pane_read_cancels_alt_screen_history_capture_before_serving_live_content() {
        with_terminal_session_test_server(
            |server, terminal_id, terminal_id_string, public_pane_id| {
                let runtime = server
                    .app
                    .terminal_runtimes
                    .get(&terminal_id)
                    .expect("runtime");
                runtime.test_process_pty_bytes(
                    b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[2J\x1b[Hcomposer-old",
                );
                let (_, initial) = runtime.screen_text_snapshot().expect("alternate screen");

                let (baseline_tx, baseline_rx) = std::sync::mpsc::channel();
                server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                    request: api::schema::Request {
                        id: "baseline".into(),
                        method: api::schema::Method::PaneRead(api::schema::PaneReadParams {
                            pane_id: public_pane_id.clone(),
                            source: api::schema::ReadSource::Visible,
                            lines: None,
                            format: api::schema::ReadFormat::Text,
                            strip_ansi: true,
                            intent: api::schema::ReadIntent::Interactive,
                        }),
                    },
                    respond_to: baseline_tx,
                    response_write_complete: None,
                    stream_active: None,
                });
                let baseline: api::schema::SuccessResponse = serde_json::from_str(
                    &baseline_rx
                        .recv_timeout(Duration::from_millis(500))
                        .expect("baseline pane read within 500 ms"),
                )
                .expect("baseline response");
                let api::schema::ResponseResult::PaneRead { read: baseline } = baseline.result
                else {
                    panic!("expected baseline pane read");
                };

                let (history_tx, _history_rx) = std::sync::mpsc::channel();
                server.pending_alt_screen_reads.push(
                    crate::server::alt_screen_read::PendingAltScreenRead::start(
                        terminal_id.clone(),
                        "history".into(),
                        history_tx,
                        "fallback".into(),
                        api::schema::PaneReadResult {
                            pane_id: public_pane_id.clone(),
                            terminal_id: terminal_id_string.clone(),
                            workspace_id: "w1".into(),
                            tab_id: "w1:t1".into(),
                            agent_ref: None,
                            agent_session: None,
                            source: api::schema::ReadSource::Recent,
                            format: api::schema::ReadFormat::Text,
                            text: String::new(),
                            revision: baseline.revision,
                            truncated: false,
                            input_observation: None,
                        },
                        200,
                        false,
                        initial,
                        Instant::now(),
                    ),
                );

                server
                    .app
                    .terminal_runtimes
                    .get(&terminal_id)
                    .expect("runtime")
                    .test_process_pty_bytes(b"\x1b[2J\x1b[Hcomposer-new");

                let (read_tx, read_rx) = std::sync::mpsc::channel();
                server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                    request: api::schema::Request {
                        id: "live".into(),
                        method: api::schema::Method::PaneRead(api::schema::PaneReadParams {
                            pane_id: public_pane_id,
                            source: api::schema::ReadSource::Recent,
                            lines: Some(200),
                            format: api::schema::ReadFormat::Text,
                            strip_ansi: true,
                            intent: api::schema::ReadIntent::Interactive,
                        }),
                    },
                    respond_to: read_tx,
                    response_write_complete: None,
                    stream_active: None,
                });
                let response: api::schema::SuccessResponse = serde_json::from_str(
                    &read_rx
                        .recv_timeout(Duration::from_millis(500))
                        .expect("live pane read within 500 ms"),
                )
                .expect("live response");
                let api::schema::ResponseResult::PaneRead { read } = response.result else {
                    panic!("expected live pane read");
                };

                assert!(read.text.contains("composer-new"), "{read:?}");
                assert!(!read.text.contains("composer-old"), "{read:?}");
                assert!(read.revision > baseline.revision, "{read:?}");
            },
        );
    }

    #[test]
    fn terminal_control_rejects_second_controller_without_takeover() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    agent_ref: None,
                    expected_context: None,
                    takeover: false,
                })
            );

            connect_pending_terminal_client(server, 8);
            assert!(
                !server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 8,
                    target: terminal_id_string.clone(),
                    agent_ref: None,
                    expected_context: None,
                    takeover: false,
                })
            );

            assert!(server.clients.contains_key(&7));
            assert!(!server.clients.contains_key(&8));
            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&7)
            );
        });
    }

    #[test]
    fn terminal_control_takeover_replaces_existing_controller() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    agent_ref: None,
                    expected_context: None,
                    takeover: false,
                })
            );

            connect_pending_terminal_client(server, 8);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 8,
                    target: terminal_id_string.clone(),
                    agent_ref: None,
                    expected_context: None,
                    takeover: true,
                })
            );

            assert!(!server.clients.contains_key(&7));
            assert!(server.clients.contains_key(&8));
            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&8)
            );
        });
    }

    #[test]
    fn terminal_observe_can_coexist_with_terminal_control() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    agent_ref: None,
                    expected_context: None,
                    takeover: false,
                })
            );

            connect_pending_terminal_client(server, 8);
            assert!(
                server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 8,
                    target: terminal_id_string.clone(),
                })
            );

            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&7)
            );
            assert!(matches!(
                server.clients.get(&8).map(|client| &client.mode),
                Some(ClientConnectionMode::TerminalObserve { terminal_id })
                    if terminal_id == &terminal_id_string
            ));
            assert_eq!(
                terminal_stream_client_ids(&server.clients, &terminal_id_string).len(),
                2
            );
        });
    }

    #[test]
    fn terminal_control_detach_sends_shutdown_before_removal() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            let control_rx = connect_pending_terminal_client_with_control_rx(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    agent_ref: None,
                    expected_context: None,
                    takeover: false,
                })
            );

            assert!(server.handle_server_event(ServerEvent::ClientDetach { client_id: 7 }));

            assert!(!server.clients.contains_key(&7));
            assert!(!server
                .terminal_attach_owners
                .contains_key(&terminal_id_string));
            let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
            assert_eq!(reason, Some("detached".to_owned()));
        });
    }

    #[test]
    fn terminal_observe_rejects_later_attach_upgrade() {
        with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                })
            );
            assert!(
                !server.handle_server_event(ServerEvent::ClientAttachTerminal {
                    client_id: 7,
                    terminal_id: terminal_id_string,
                    takeover: true,
                })
            );

            assert!(!server.clients.contains_key(&7));
            assert!(server.terminal_attach_owners.is_empty());
            assert!(!server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));
        });
    }

    #[test]
    fn terminal_attach_rejects_later_observe_and_clears_ownership() {
        with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientAttachTerminal {
                    client_id: 7,
                    terminal_id: terminal_id_string.clone(),
                    takeover: false,
                })
            );
            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&7)
            );
            assert!(server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));

            assert!(
                !server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                })
            );

            assert!(!server.clients.contains_key(&7));
            assert!(server.terminal_attach_owners.is_empty());
            assert!(!server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));
        });
    }

    fn app_client_marks_git_refresh_due_on_first_attach(render_encoding: RenderEncoding) {
        let mut server = test_headless_server();
        server
            .app
            .state
            .workspaces
            .push(crate::workspace::Workspace::test_new("test"));
        let future = Instant::now() + Duration::from_secs(60);
        server.app.last_git_remote_status_refresh = future;
        let (writer, _control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        assert!(server.has_app_client());
        assert!(server
            .app
            .git_refresh_deadline()
            .is_some_and(|deadline| deadline <= Instant::now()));
    }

    #[test]
    fn terminal_ansi_app_client_enables_headless_git_refresh() {
        app_client_marks_git_refresh_due_on_first_attach(RenderEncoding::TerminalAnsi);
    }

    #[test]
    fn pending_terminal_attach_client_does_not_enable_headless_git_refresh() {
        let mut server = test_headless_server();
        server
            .app
            .state
            .workspaces
            .push(crate::workspace::Workspace::test_new("test"));
        let (writer, _control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            direct_graphics: false,
            writer,
        }));

        assert!(!server.has_app_client());
        assert_eq!(
            server.app.next_headless_loop_deadline_with_client_refresh(
                Instant::now(),
                false,
                server.has_app_client()
            ),
            None
        );
    }

    #[test]
    fn writerless_app_client_does_not_enable_headless_git_refresh() {
        let mut server = test_headless_server();
        server
            .app
            .state
            .workspaces
            .push(crate::workspace::Workspace::test_new("test"));
        let (writer, _control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));
        assert!(server.has_app_client());

        server.clients.get_mut(&7).expect("client").writer = None;

        assert!(!server.has_app_client());
        assert_eq!(
            server.app.next_headless_loop_deadline_with_client_refresh(
                Instant::now(),
                false,
                server.has_app_client()
            ),
            None
        );
    }

    #[test]
    fn semantic_app_client_marks_git_refresh_due_on_first_attach() {
        app_client_marks_git_refresh_due_on_first_attach(RenderEncoding::SemanticFrame);
    }

    /// The headless server runs its own scheduler, so a nudge armed by a native
    /// resume only fires if that scheduler ticks it. Without the tick in
    /// `handle_scheduled_tasks_headless` this passes in the TUI and does nothing
    /// behind `herdr server`, which is how #273 shipped inert in 4fa86f16.
    #[tokio::test]
    async fn headless_scheduler_fires_a_pending_resume_nudge() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("headless-resume-nudge");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 4,
            );
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        server.app.arm_resume_nudge(pane_id, &terminal_id, "claude");

        let armed_at = Instant::now();
        server.handle_scheduled_tasks_headless(armed_at, false);
        assert!(
            server.app.pending_resume_nudges.contains_key(&terminal_id),
            "the nudge should still be waiting out its idle hold"
        );

        server.handle_scheduled_tasks_headless(
            armed_at + crate::app::agent_resume::RESUME_NUDGE_IDLE_HOLD,
            false,
        );

        assert!(
            !server.app.pending_resume_nudges.contains_key(&terminal_id),
            "the headless scheduler never ticked the resume nudge"
        );
        let mut sent = String::new();
        while let Ok(bytes) = rx.try_recv() {
            sent.push_str(&String::from_utf8_lossy(&bytes));
        }
        assert!(
            sent.contains("continue"),
            "expected the nudge to reach the pane, got {sent:?}"
        );
    }

    #[tokio::test]
    async fn headless_scheduler_fires_a_stalled_agent_auto_nudge() {
        let now = Instant::now();
        let mut server = test_headless_server();
        server.app.state.auto_nudge_stalled_agents = true;
        let mut workspace = crate::workspace::Workspace::test_new("headless-auto-nudge");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        workspace.tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - server.app.state.nudge_after);
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        let terminal = server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.supervisor_stale = true;
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 4,
            );
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);

        server.handle_scheduled_tasks_headless(now, false);

        assert!(server.app.stall_nudge_episodes.contains_key(&terminal_id));
        let mut sent = String::new();
        while let Ok(bytes) = rx.try_recv() {
            sent.push_str(&String::from_utf8_lossy(&bytes));
        }
        assert!(
            sent.contains("Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue."),
            "expected the auto-nudge to reach the pane, got {sent:?}"
        );
    }

    #[tokio::test]
    async fn headless_scheduler_working_report_uses_the_busy_budget() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("headless-working-watchdog");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.app.handle_internal_event_with_prefix_sync(
            crate::events::AppEvent::HookStateReported {
                pane_id,
                source: "herdr:claude-closing-block".into(),
                agent_label: "claude".into(),
                state: crate::detect::AgentState::Working,
                message: None,
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                session_ref: None,
                closing_block: None,
            },
        );
        let reported_at = server.app.state.terminals[&terminal_id]
            .status_reported_at()
            .expect("working report timestamp");
        server.handle_scheduled_tasks_headless(
            reported_at + server.app.state.agent_stale_after,
            false,
        );
        assert!(!server.app.state.terminals[&terminal_id].supervisor_stale);

        server.handle_scheduled_tasks_headless(
            reported_at + crate::terminal::state::AGENT_BUSY_STALE_SILENCE - Duration::from_secs(1),
            false,
        );
        assert!(!server.app.state.terminals[&terminal_id].supervisor_stale);

        server.handle_scheduled_tasks_headless(
            reported_at + crate::terminal::state::AGENT_BUSY_STALE_SILENCE,
            false,
        );
        assert!(server.app.state.terminals[&terminal_id].supervisor_stale);
    }

    #[tokio::test]
    async fn headless_scheduler_declared_wait_uses_eta_plus_grace() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("headless-declared-wait-watchdog");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.app.handle_internal_event_with_prefix_sync(
            crate::events::AppEvent::HookStateReported {
                pane_id,
                source: "herdr:claude-closing-block".into(),
                agent_label: "claude".into(),
                state: crate::detect::AgentState::Working,
                message: None,
                seq: Some(1),
                wait: Some("CI run 4123".into()),
                eta_s: Some(120),
                reported_at: None,
                session_ref: None,
                closing_block: None,
            },
        );
        let reported_at = server.app.state.terminals[&terminal_id]
            .status_reported_at()
            .expect("declared wait report timestamp");
        let stale_at = reported_at
            .checked_add(Duration::from_secs(120))
            .and_then(|deadline| deadline.checked_add(crate::terminal::state::DECLARED_WAIT_GRACE))
            .expect("declared wait watchdog deadline");

        assert_eq!(
            server.app.state.next_agent_watchdog_deadline(),
            Some(stale_at)
        );
        server.handle_scheduled_tasks_headless(stale_at - Duration::from_secs(1), false);
        assert!(!server.app.state.terminals[&terminal_id].supervisor_stale);

        server.handle_scheduled_tasks_headless(stale_at, false);

        assert!(server.app.state.terminals[&terminal_id].supervisor_stale);
    }

    #[tokio::test]
    async fn headless_scheduler_finished_report_ignores_a_lingering_subprocess() {
        let now = Instant::now();
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("headless-subprocess-watchdog");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        let terminal = server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            crate::detect::AgentState::Idle,
            None,
            None,
            Some(1),
            now,
        );
        terminal.set_foreground_process(Some("cargo".into()), true, now);

        assert_eq!(server.app.state.next_agent_watchdog_deadline(), None);
        server.handle_scheduled_tasks_headless(
            now + server.app.state.agent_stale_after + Duration::from_secs(1),
            false,
        );
        assert!(!server.app.state.terminals[&terminal_id].supervisor_stale);
    }

    #[tokio::test]
    async fn headless_scheduler_subagent_claim_uses_configured_budget() {
        let now = Instant::now();
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("headless-subagent-watchdog");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        let terminal = server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            crate::detect::AgentState::Idle,
            None,
            None,
            Some(1),
            now,
        );
        terminal.set_active_subagents(Some(1));

        server.handle_scheduled_tasks_headless(
            now + server.app.state.agent_subagent_stale_after - Duration::from_secs(1),
            false,
        );
        assert!(!server.app.state.terminals[&terminal_id].supervisor_stale);

        server.handle_scheduled_tasks_headless(
            now + server.app.state.agent_subagent_stale_after,
            false,
        );
        assert!(server.app.state.terminals[&terminal_id].supervisor_stale);
    }

    /// AC6: terminal-attach draft bytes suppress a stalled-agent auto-nudge.
    #[tokio::test]
    async fn headless_attach_human_bytes_suppress_a_stalled_agent_auto_nudge() {
        let now = Instant::now();
        let mut server = test_headless_server();
        server.app.state.auto_nudge_stalled_agents = true;
        let workspace = crate::workspace::Workspace::test_new("headless-attach-draft");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        let terminal = server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.supervisor_stale = true;
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 4,
            );
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);

        let result = server
            .forward_terminal_attach_bytes(&terminal_id.to_string(), b"draft".to_vec(), false)
            .expect("terminal target");
        assert!(result.is_ok());
        assert_eq!(rx.try_recv().expect("attached bytes"), Bytes::from("draft"));
        let nudge_after = server.app.state.nudge_after;
        server.app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - nudge_after);
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .supervisor_stale = true;

        server.handle_scheduled_tasks_headless(now, false);

        assert!(
            rx.try_recv().is_err(),
            "auto-nudge wrote into a human draft"
        );
    }

    /// The animation ships through the server loop, not `App::run`, so this is
    /// the path that has to advance it. It shipped ticking only in `App::run`,
    /// which is why the field stood still in front of every real client.
    #[test]
    fn an_attached_headless_server_advances_the_sidebar_animation() {
        let mut server = test_headless_server();
        server.app.state.hyperspace.enabled = true;
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 120,
            rows: 40,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));
        crate::ui::compute_view(
            &mut server.app.state,
            ratatui::layout::Rect::new(0, 0, 120, 40),
        );
        assert!(
            server.app.state.view.hyperspace_rect.height > 0,
            "the panel has to be on screen for the tick to mean anything"
        );

        let before = server.app.state.hyperspace.step();
        let now = Instant::now() + crate::hyperspace::FRAME_INTERVAL * 2;
        assert!(
            server.handle_scheduled_tasks_headless(now, false),
            "advancing the field is a render-worthy change"
        );
        assert_ne!(
            server.app.state.hyperspace.step(),
            before,
            "the star field has to move"
        );
    }

    #[test]
    fn a_detached_headless_server_leaves_the_sidebar_animation_alone() {
        let mut server = test_headless_server();
        server.app.state.hyperspace.enabled = true;
        let before = server.app.state.hyperspace.step();

        server.handle_scheduled_tasks_headless(
            Instant::now() + crate::hyperspace::FRAME_INTERVAL * 2,
            false,
        );

        assert_eq!(server.app.state.hyperspace.step(), before);
    }

    #[test]
    fn detached_headless_server_does_not_start_status_metric_sampling() {
        let mut server = test_headless_server();
        server.app.status_metric_refresh_enabled = true;

        server.handle_scheduled_tasks_headless(Instant::now(), false);

        assert!(!server.app.status_metric_refresh.in_flight());
    }

    #[test]
    fn headless_status_side_signals_require_a_renderable_app_client() {
        let mut detached = test_headless_server();
        detached.app.status_metric_refresh_enabled = true;
        detached.app.provider_usage_refreshed_at = None;
        detached.app.connectivity_probed_at = None;
        let detached_now = Instant::now();

        detached.handle_scheduled_tasks_headless(detached_now, false);

        assert_eq!(detached.app.provider_usage_refreshed_at, None);
        assert!(!detached.app.provider_usage_in_flight);
        assert_eq!(detached.app.connectivity_probed_at, None);
        assert!(!detached.app.connectivity_probe_in_flight);

        let mut attached = test_headless_server();
        attached.app.status_metric_refresh_enabled = true;
        attached.app.provider_usage_refreshed_at = None;
        attached.app.connectivity_probed_at = None;
        let now = Instant::now();
        assert!(attached.app.status_metric_refresh.begin(now));
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(attached.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 120,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        attached.handle_scheduled_tasks_headless(now, false);

        assert!(attached.app.status_metrics_visible);
        assert_eq!(attached.app.provider_usage_refreshed_at, Some(now));
        assert!(attached.app.provider_usage_in_flight);
        assert_eq!(attached.app.connectivity_probed_at, Some(now));
        assert!(attached.app.connectivity_probe_in_flight);
    }

    #[test]
    fn a_notepad_client_refreshes_usage_when_the_status_bar_is_disabled() {
        let mut server = test_headless_server();
        server.app.status_metric_refresh_enabled = true;
        server.app.state.status_bar_enabled = false;
        server.app.state.notepad.enabled = true;
        let now = Instant::now();
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));
        server.app.provider_usage_refreshed_at = None;
        server.app.provider_usage_in_flight = false;
        server.app.connectivity_probed_at = None;
        server.app.connectivity_probe_in_flight = false;

        server.handle_scheduled_tasks_headless(now, false);

        assert_eq!(server.app.provider_usage_refreshed_at, Some(now));
        assert!(server.app.provider_usage_in_flight);
        assert_eq!(server.app.connectivity_probed_at, None);
        assert!(!server.app.connectivity_probe_in_flight);
    }

    #[test]
    fn detached_headless_server_discards_client_activity_refresh_deadline() {
        let mut server = test_headless_server();
        let now = Instant::now();
        server.app.agent_activity_refresh_deadline =
            Some(now.checked_sub(Duration::from_secs(1)).expect("deadline"));

        assert!(!server.handle_scheduled_tasks_headless(now, false));
        assert!(server.app.agent_activity_refresh_deadline.is_none());
    }

    #[test]
    fn headless_space_suffix_keeps_age_frame_schedule() {
        let mut server = test_headless_server();
        server.app.state.status_bar_enabled = false;
        server.app.state.mobile_width_threshold = 0;
        server.app.state.sidebar_width = 42;
        server.app.state.sidebar_min_width = 18;
        server.app.state.sidebar_max_width = 120;
        let mut workspace = crate::workspace::Workspace::test_new("clock-space");
        workspace.tabs[0].custom_name = Some("Clock task".into());
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let started = Instant::now() - Duration::from_secs(119);
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Working,
                false,
                false,
                true,
                false,
                false,
                started,
            );

        let (writer, _control_rx, render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 100,
            rows: 20,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));
        let presentation = &mut server
            .clients
            .get_mut(&7)
            .expect("connected clock client")
            .sidebar_presentation;
        presentation.group_mode = crate::app::state::SidebarGroupMode::Repo;
        presentation.work_filter.query.clear();
        server.render_and_stream();
        let first = read_server_frame(
            render_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial clock frame"),
        );
        let first_text = frame_text(&first);
        assert!(first_text.contains("Clock task"), "{first_text:?}");
        assert!(first_text.contains("1m"), "{first_text:?}");
        assert!(!first_text.contains("ago"), "{first_text:?}");
        assert!(server.app.agent_activity_refresh_deadline.is_some());
        assert!(!server.handle_scheduled_tasks_headless(Instant::now(), false));
    }

    #[test]
    fn attached_app_client_starts_status_metric_sampling_after_first_render() {
        let mut server = test_headless_server();
        server.app.status_metric_refresh_enabled = true;
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 120,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        server.handle_scheduled_tasks_headless(Instant::now(), true);
        assert!(!server.app.status_metric_refresh.in_flight());

        server.render_and_stream();
        server.handle_scheduled_tasks_headless(Instant::now(), false);

        assert!(server.app.status_metric_refresh.in_flight());
    }

    #[test]
    fn narrow_to_wide_resize_waits_for_new_geometry_frame_before_sampling() {
        let mut server = test_headless_server();
        server.app.status_metric_refresh_enabled = true;
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 40,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));
        server.render_and_stream();
        server.handle_scheduled_tasks_headless(Instant::now(), false);
        assert!(!server.app.status_metric_refresh.in_flight());

        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 7,
            cols: 120,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
        }));
        server.handle_scheduled_tasks_headless(Instant::now(), true);
        assert!(!server.app.status_metric_refresh.in_flight());

        server.render_and_stream();
        server.handle_scheduled_tasks_headless(Instant::now(), false);
        assert!(server.app.status_metric_refresh.in_flight());
    }

    #[test]
    fn status_sampling_tracks_renderable_clients() {
        let mut server = test_headless_server();
        server.app.status_metric_refresh_enabled = true;
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 120,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        server.handle_scheduled_tasks_headless(Instant::now(), false);

        assert!(server.app.status_metrics_visible);
        assert!(server.app.status_metric_refresh.in_flight());

        server.app.status_metric_refresh.finish();
        server
            .clients
            .get_mut(&7)
            .expect("attached client")
            .terminal_size = (40, 24);
        server.handle_scheduled_tasks_headless(
            Instant::now() + crate::platform::status_metrics::STATUS_METRIC_REFRESH_INTERVAL,
            false,
        );

        assert!(!server.app.status_metrics_visible);
        assert!(!server.app.status_metric_refresh.in_flight());
    }

    #[test]
    fn unchanged_git_refresh_does_not_request_headless_render() {
        let mut server = test_headless_server();
        server.app.test_begin_git_refresh(1);
        let mut workspace = crate::workspace::Workspace::test_new("one");
        let workspace_id = workspace.id.clone();
        let cwd = workspace.identity_cwd.clone();
        workspace.cached_auto_label = "cached".into();
        workspace.cached_git_status_key = cwd.clone();
        workspace.cached_git_branch = None;
        server.app.state.workspaces.push(workspace);

        let changed = server.handle_internal_event_with_forwarding(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: vec![crate::workspace::WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd.clone(),
                status_cache_key: cwd,
                demand: crate::workspace::GitStatusRefreshDemand::ALL,
                updates_workspace_identity: true,
                auto_label: "cached".into(),
                branch: None,
                ahead_behind: None,
                space: None,
            }],
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert!(!changed);
        assert!(server.app.git_refresh_in_flight.is_none());
    }

    #[test]
    fn changed_git_refresh_requests_headless_render() {
        let mut server = test_headless_server();
        server.app.test_begin_git_refresh(1);
        let workspace = crate::workspace::Workspace::test_new("one");
        let workspace_id = workspace.id.clone();
        let cwd = workspace.identity_cwd.clone();
        server.app.state.workspaces.push(workspace);

        let changed = server.handle_internal_event_with_forwarding(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: vec![crate::workspace::WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd.clone(),
                status_cache_key: cwd,
                demand: crate::workspace::GitStatusRefreshDemand::ALL,
                updates_workspace_identity: true,
                auto_label: "one".into(),
                branch: Some("changed".into()),
                ahead_behind: None,
                space: None,
            }],
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert!(changed);
    }

    #[test]
    fn terminal_attach_client_exits_when_attached_pane_dies() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("attached");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane")
            .attached_terminal_id
            .to_string();
        let (writer, control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            direct_graphics: false,
            writer,
        }));
        assert!(
            server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 7,
                terminal_id: terminal_id.clone(),
                takeover: false,
            })
        );
        assert_eq!(server.terminal_attach_owners.get(&terminal_id), Some(&7));

        assert!(server.handle_internal_event_with_forwarding(AppEvent::PaneDied { pane_id }));

        assert!(!server.clients.contains_key(&7));
        assert!(!server.terminal_attach_owners.contains_key(&terminal_id));
        let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
        assert_eq!(reason, Some(format!("terminal {terminal_id} exited")));
    }

    #[test]
    fn terminal_attach_scroll_moves_attached_runtime_viewport() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 4096, &bytes);

        apply_terminal_attach_scroll(
            &runtime,
            AttachScrollSource::Wheel,
            AttachScrollDirection::Up,
            3,
            None,
            None,
            0,
        )
        .expect("scroll up");
        let metrics = runtime.scroll_metrics().expect("scroll metrics");
        assert_eq!(metrics.offset_from_bottom, 3);

        apply_terminal_attach_scroll(
            &runtime,
            AttachScrollSource::Wheel,
            AttachScrollDirection::Down,
            2,
            None,
            None,
            0,
        )
        .expect("scroll down");
        let metrics = runtime.scroll_metrics().expect("scroll metrics");
        assert_eq!(metrics.offset_from_bottom, 1);
        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn terminal_attach_input_resets_scrolled_viewport() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                20, 5, 4096, &bytes, 4,
            );

        runtime.scroll_up(4);
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            4
        );

        apply_terminal_attach_input(&runtime, b"x".to_vec()).expect("attach input");
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded input"),
            Bytes::from("x")
        );

        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[tokio::test]
    async fn terminal_attach_client_input_retires_blocked_hook_authority_after_forwarding() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("attached");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal_id_string = terminal_id.to_string();
        let (runtime, mut input_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Codex),
            crate::detect::AgentState::Idle,
        );
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            crate::detect::AgentState::Blocked,
            None,
            Some(1),
        );

        let mut client = test_app_client(Some(true), 1);
        client.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: terminal_id_string,
            control: None,
        };
        server.clients.insert(1, client);

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"continue".to_vec(),
        }));

        assert_eq!(
            input_rx.try_recv().expect("forwarded attach input"),
            Bytes::from_static(b"continue")
        );
        assert_eq!(
            server.app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );
        assert!(!server.app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
    }

    fn with_terminal_attach_runtime(
        initial_bytes: &[u8],
        initial_scroll: usize,
        test: impl FnOnce(&crate::terminal::TerminalRuntime, &mut mpsc::Receiver<Bytes>),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = initial_bytes.to_vec();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                20, 5, 4096, &bytes, 4,
            );
        if initial_scroll > 0 {
            runtime.scroll_up(initial_scroll);
        }

        test(&runtime, &mut input_rx);

        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    fn apply_terminal_attach_page_up(runtime: &crate::terminal::TerminalRuntime) {
        apply_terminal_attach_scroll(
            runtime,
            AttachScrollSource::PageKey {
                input: b"\x1b[5~".to_vec(),
            },
            AttachScrollDirection::Up,
            4,
            None,
            None,
            0,
        )
        .expect("page key");
    }

    #[test]
    fn terminal_attach_paste_uses_plain_text_when_runtime_did_not_enable_brackets() {
        with_terminal_attach_runtime(b"", 0, |runtime, input_rx| {
            apply_terminal_attach_input(runtime, b"\x1b[200~line one\nline two\x1b[201~".to_vec())
                .expect("attach paste");

            assert_eq!(
                input_rx.try_recv().expect("forwarded paste"),
                Bytes::from_static(b"line one\nline two")
            );
        });
    }

    #[test]
    fn terminal_attach_paste_preserves_brackets_when_runtime_enabled_them() {
        with_terminal_attach_runtime(b"\x1b[?2004h", 0, |runtime, input_rx| {
            apply_terminal_attach_input(runtime, b"\x1b[200~line one\nline two\x1b[201~".to_vec())
                .expect("attach paste");

            assert_eq!(
                input_rx.try_recv().expect("forwarded paste"),
                Bytes::from_static(b"\x1b[200~line one\nline two\x1b[201~")
            );
        });
    }

    #[test]
    fn terminal_attach_page_key_host_scrolls_plain_terminal() {
        with_terminal_attach_runtime(b"", 0, |runtime, input_rx| {
            apply_terminal_attach_page_up(runtime);

            assert_eq!(
                runtime
                    .scroll_metrics()
                    .expect("scroll metrics")
                    .offset_from_bottom,
                4
            );
            assert!(input_rx.try_recv().is_err());
        });
    }

    #[test]
    fn terminal_attach_page_key_forwards_when_mouse_reporting() {
        with_terminal_attach_runtime(b"\x1b[?1000h", 3, |runtime, input_rx| {
            apply_terminal_attach_page_up(runtime);

            assert_eq!(
                runtime
                    .scroll_metrics()
                    .expect("scroll metrics")
                    .offset_from_bottom,
                0
            );
            assert_eq!(
                input_rx.try_recv().expect("forwarded page key"),
                Bytes::from_static(b"\x1b[5~")
            );
        });
    }

    #[test]
    fn terminal_attach_page_key_forwards_when_application_cursor() {
        with_terminal_attach_runtime(b"\x1b[?1h", 3, |runtime, input_rx| {
            apply_terminal_attach_page_up(runtime);

            assert_eq!(
                runtime
                    .scroll_metrics()
                    .expect("scroll metrics")
                    .offset_from_bottom,
                0
            );
            assert_eq!(
                input_rx.try_recv().expect("forwarded page key"),
                Bytes::from_static(b"\x1b[5~")
            );
        });
    }

    #[test]
    fn terminal_attach_page_key_host_scrolls_shell_like_decckm_with_bracketed_paste() {
        with_terminal_attach_runtime(b"\x1b[?1h\x1b[?2004h", 0, |runtime, input_rx| {
            apply_terminal_attach_page_up(runtime);

            assert_eq!(
                runtime
                    .scroll_metrics()
                    .expect("scroll metrics")
                    .offset_from_bottom,
                4
            );
            assert!(input_rx.try_recv().is_err());
        });
    }

    #[test]
    fn terminal_attach_page_key_forwards_in_alternate_screen_without_mouse_reporting() {
        with_terminal_attach_runtime(b"\x1b[?1049h", 3, |runtime, input_rx| {
            apply_terminal_attach_page_up(runtime);

            assert_eq!(
                runtime
                    .scroll_metrics()
                    .expect("scroll metrics")
                    .offset_from_bottom,
                0
            );
            assert_eq!(
                input_rx.try_recv().expect("forwarded page key"),
                Bytes::from_static(b"\x1b[5~")
            );
        });
    }

    #[test]
    fn headless_scheduled_tasks_expire_agent_metadata() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("metadata");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::HookStateReported {
                pane_id,
                source: "custom:pi".into(),
                agent_label: "pi".into(),
                state: crate::detect::AgentState::Working,
                message: None,
                seq: None,
                wait: None,
                eta_s: None,
                reported_at: None,
                session_ref: None,
                closing_block: None,
            })
        );
        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::HookMetadataReported {
                pane_id,
                source: "user:pi-display".into(),
                agent_label: Some("pi".into()),
                applies_to_source: Some("custom:pi".into()),
                title: Some("short lived".into()),
                display_agent: None,
                state_labels: HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                seq: None,
                // Expiry is advanced with the captured deadline below; keep the
                // pre-expiry assertion independent of wall-clock scheduling.
                ttl: Some(Duration::from_secs(60)),
            })
        );

        let deadline = server
            .app
            .agent_metadata_deadline
            .expect("metadata deadline");
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane")
            .attached_terminal_id
            .clone();
        assert_eq!(
            server
                .app
                .state
                .terminals
                .get(&terminal_id)
                .expect("terminal")
                .effective_title()
                .as_deref(),
            Some("short lived")
        );

        assert!(server.handle_scheduled_tasks_headless(deadline + Duration::from_millis(1), false));

        assert_eq!(server.app.agent_metadata_deadline, None);
        assert_eq!(
            server
                .app
                .state
                .terminals
                .get(&terminal_id)
                .expect("terminal")
                .effective_title(),
            None
        );
        assert!(server
            .app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| {
                event.event == crate::api::schema::EventKind::PaneAgentStatusChanged
                    && matches!(
                        &event.data,
                        crate::api::schema::EventData::PaneAgentStatusChanged {
                            title,
                            ..
                        } if title.is_none()
                    )
            }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_full_lifecycle_hook_authority_falls_back_to_screen_in_headless_scheduler() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("hook-expiry")];
        server.app.state.ensure_test_terminals();
        let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = server.app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let reported_at = Instant::now();
        let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Kimi),
            crate::detect::AgentState::Idle,
        );
        let session_ref = crate::agent_resume::AgentSessionRef::id("headless-expiry").unwrap();
        terminal.set_agent_session_ref_for_session_start(
            "herdr:kimi".into(),
            "kimi".into(),
            Some(session_ref.clone()),
            Some(1),
            Some("startup".into()),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            crate::detect::AgentState::Working,
            None,
            Some(session_ref),
            Some(2),
            reported_at,
        );
        server.app.terminal_runtimes.insert(
            terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        server.app.sync_detection_authority_mirrors();
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .hook_authority_runtime_state_for_test(),
            (true, false)
        );
        let deadline = server
            .app
            .state
            .next_full_lifecycle_hook_authority_deadline()
            .unwrap();
        let sequence = server.app.event_hub.current_sequence();

        assert!(server.handle_scheduled_tasks_headless(deadline, false));
        assert_eq!(
            server.app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );
        assert!(!server.app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .hook_authority_runtime_state_for_test(),
            (false, false)
        );
        assert!(server
            .app
            .event_hub
            .events_after(sequence)
            .iter()
            .any(|(_, event)| { event.event == crate::api::schema::EventKind::PaneUpdated }));
        assert!(server
            .app
            .state
            .next_full_lifecycle_hook_authority_deadline()
            .is_none());
    }

    #[test]
    fn headless_scheduled_tasks_clears_disabled_agent_manifest_update_deadline() {
        let mut server = test_headless_server();
        let now = Instant::now();
        server.app.next_agent_manifest_update_check = Some(now - Duration::from_millis(1));

        assert!(!server.handle_scheduled_tasks_headless(now, false));
        assert_eq!(server.app.next_agent_manifest_update_check, None);
    }

    #[test]
    fn headless_scheduled_tasks_refresh_foreground_processes_without_app_client() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("api-only")];
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0].tabs[0]
            .terminal_id(server.app.state.workspaces[0].tabs[0].root_pane)
            .cloned()
            .expect("test pane terminal");
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        let now = Instant::now();
        server.app.next_foreground_process_refresh = now;

        assert!(!server.has_app_client());
        server.handle_scheduled_tasks_headless(now, false);

        assert_eq!(server.app.last_foreground_process_refresh_generation, 1);
        assert!(server.app.foreground_process_refresh_in_flight.is_some());
    }

    #[test]
    fn headless_scheduled_tasks_skip_foreground_refresh_for_non_agent_panes() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("api-only")];
        server.app.state.ensure_test_terminals();
        let now = Instant::now();
        server.app.next_foreground_process_refresh = now;

        assert!(!server.has_app_client());
        server.handle_scheduled_tasks_headless(now, false);

        assert_eq!(server.app.last_foreground_process_refresh_generation, 0);
        assert!(server.app.foreground_process_refresh_in_flight.is_none());
    }

    #[tokio::test]
    async fn headless_scheduled_tasks_do_not_start_pending_agent_resume_when_geometry_dirty() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        server.app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                server.app.state.host_terminal_theme,
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.effective_size = (100, 30);
        server.app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });
        server.app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        assert!(!server.handle_scheduled_tasks_headless(Instant::now(), true));
        assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(server.app.pending_agent_resume_deadline.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn headless_scheduled_tasks_start_pending_agent_resume_without_foreground_client() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        server.render_and_stream();
        assert_ne!(server.app.state.view.terminal_area, Rect::default());

        let now = Instant::now();
        assert!(!server.handle_scheduled_tasks_headless(now, false));
        assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());
        let deadline = server
            .app
            .pending_agent_resume_deadline
            .expect("clientless resume should wait briefly for a host theme");

        assert!(server.handle_scheduled_tasks_headless(deadline, false));
        assert!(server.app.terminal_runtimes.get(&terminal_id).is_some());
        assert!(server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_none());
        shutdown_test_runtimes(&mut server);
    }

    #[tokio::test]
    async fn headless_pre_input_resize_does_not_start_pending_agent_resume() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        server.app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                server.app.state.host_terminal_theme,
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.effective_size = (100, 30);
        server.app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });
        server.app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        server.resize_shared_runtime_to_effective_size_before_input();

        assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(server.app.pending_agent_resume_deadline.is_none());
    }

    #[test]
    fn virtual_render_produces_nonempty_buffer() {
        let mut state = AppState::test_new();
        let area = Rect::new(0, 0, 80, 24);
        let (buffer, _cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
        assert_eq!(buffer.area.width, 80);
        assert_eq!(buffer.area.height, 24);
    }

    #[test]
    fn virtual_render_without_frame_cursor_keeps_cursor_hidden() {
        let mut state = AppState::test_new();
        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert_eq!(cursor, None);
    }

    #[tokio::test]
    async fn virtual_render_preserves_explicit_frame_cursor_position() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
        let pane = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("focused pane info");

        assert_eq!(
            cursor,
            Some(CursorState {
                x: pane.inner_rect.x + 4,
                y: pane.inner_rect.y,
                visible: true,
                shape: cursor.as_ref().map(|c| c.shape).unwrap_or(0),
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_preserves_hidden_focused_pane_cursor_position() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
        let pane = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("focused pane info");

        assert_eq!(
            cursor,
            Some(CursorState {
                x: pane.inner_rect.x + 4,
                y: pane.inner_rect.y,
                visible: false,
                shape: cursor.as_ref().map(|c| c.shape).unwrap_or(0),
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_hides_focused_pane_cursor_during_synchronized_output() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left");
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.test_process_pty_bytes(b"\x1b[?2026h\x1b[2;3H");
        assert!(runtime.synchronized_output_active());

        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, false);

        assert_eq!(
            cursor, None,
            "child cursor positions are unstable while synchronized output is active"
        );
    }

    #[tokio::test]
    async fn virtual_render_hides_focused_pane_cursor_during_synchronized_output_resize() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left");
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let initial_area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, initial_area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.test_process_pty_bytes(b"\x1b[?2026h\x1b[2;3H");
        assert!(runtime.synchronized_output_active());

        let resized_area = Rect::new(0, 0, 100, 30);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, resized_area, true);

        assert_eq!(
            cursor, None,
            "pre-resize synchronized output should suppress the cursor even if resize clears the mode"
        );
    }

    #[tokio::test]
    async fn virtual_render_exposes_hidden_pane_cursor_when_reveal_hidden_for_cjk_ime() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
        let pane = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("focused pane info");

        assert_eq!(
            cursor,
            Some(CursorState {
                x: pane.inner_rect.x + 4,
                y: pane.inner_rect.y,
                visible: true,
                shape: state.cjk_ime_cursor_shape,
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_keeps_cursor_hidden_when_scrolled_back_even_with_reveal_hidden_for_cjk_ime(
    ) {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 4096, &bytes);
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.scroll_up(6);
        assert!(crate::ui::pane_is_scrolled_back(runtime));

        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "scrolled-back focused pane should keep the cursor hidden even when reveal_hidden_cursor_for_cjk_ime is true; got {cursor:?}",
        );
    }

    #[tokio::test]
    async fn virtual_render_fallback_cursor_when_viewport_none_and_reveal_hidden_for_cjk_ime() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        // Feed only ?25l with no prior cursor movement — exercises the fallback
        // path for TUIs whose viewport has no cursor position.
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
        let pane = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("focused pane info");

        assert_eq!(
            cursor,
            Some(CursorState {
                x: pane.inner_rect.x,
                y: pane.inner_rect.y,
                visible: true,
                shape: state.cjk_ime_cursor_shape,
            }),
            "fallback should anchor at pane top-left with the configured shape",
        );
    }

    #[tokio::test]
    async fn virtual_render_skips_reveal_when_focused_pane_has_no_detected_agent() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        // Filter only Claude, but the test pane has no detected agent, so the
        // reveal must not apply.
        state.cjk_ime_agent_filter_configured = true;
        state.cjk_ime_agents = vec![crate::detect::Agent::Claude];
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "agent filter should suppress reveal when the focused pane's detected agent is not on the list; got {cursor:?}",
        );
    }

    #[tokio::test]
    async fn virtual_render_skips_reveal_when_agent_filter_has_no_valid_entries() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        state.cjk_ime_agent_filter_configured = true;
        state.cjk_ime_agents = Vec::new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "agent filter with no valid entries should suppress reveal; got {cursor:?}",
        );
    }

    #[tokio::test]
    async fn virtual_render_omits_focused_pane_cursor_while_mobile_switcher_open() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Navigate);

        let area = Rect::new(0, 0, 44, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert_eq!(cursor, None);
    }

    #[tokio::test]
    async fn virtual_render_hides_focused_pane_cursor_while_scrolled_back() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 4096, &bytes);
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(crate::app::Mode::Terminal);

        let area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.scroll_up(6);
        assert!(crate::ui::pane_is_scrolled_back(runtime));

        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "cursor: {cursor:?}"
        );
    }

    #[test]
    fn latest_active_client_drives_shared_size_theme_and_fallback() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection::new(
                (160, 45),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
                    foreground: Some(crate::terminal_theme::RgbColor {
                        r: 0xaa,
                        g: 0xbb,
                        b: 0xcc,
                    }),
                    background: Some(crate::terminal_theme::RgbColor {
                        r: 0x11,
                        g: 0x22,
                        b: 0x33,
                    }),
                    ..Default::default()
                },
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
                    foreground: Some(crate::terminal_theme::RgbColor {
                        r: 0x10,
                        g: 0x20,
                        b: 0x30,
                    }),
                    background: Some(crate::terminal_theme::RgbColor {
                        r: 0xdd,
                        g: 0xee,
                        b: 0xff,
                    }),
                    ..Default::default()
                },
                None,
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        assert!(server.promote_client_to_foreground(1));
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.effective_size, (160, 45));
        assert_eq!(
            server.app.state.host_terminal_theme,
            server.clients[&1].host_terminal_theme
        );

        assert!(server.promote_client_to_foreground(2));
        assert_eq!(server.foreground_client_id, Some(2));
        assert_eq!(server.effective_size, (80, 24));
        assert_eq!(
            server.app.state.host_terminal_theme,
            server.clients[&2].host_terminal_theme
        );

        assert!(server.remove_client(2));
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.effective_size, (160, 45));
        assert_eq!(
            server.app.state.host_terminal_theme,
            server.clients[&1].host_terminal_theme
        );
    }

    #[test]
    fn foreground_client_without_host_theme_clears_previous_host_theme() {
        let mut server = test_headless_server();
        let known_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x10,
                g: 0x20,
                b: 0x30,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x40,
                g: 0x50,
                b: 0x60,
            }),
            ..Default::default()
        };
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                known_theme,
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        assert!(server.promote_client_to_foreground(1));
        assert_eq!(server.app.state.host_terminal_theme, known_theme);

        assert!(server.promote_client_to_foreground(2));
        assert_eq!(
            server.app.state.host_terminal_theme,
            crate::terminal_theme::TerminalTheme::default()
        );
    }

    #[test]
    fn foreground_promotion_syncs_context_menu_input_source() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, control_b, _render_b) = test_client_writer();
        let mut client_a = test_app_client(Some(true), 1);
        client_a.writer = Some(writer_a);
        server.clients.insert(1, client_a);
        let mut context_owner = test_app_client(Some(true), 2);
        context_owner.writer = Some(writer_b);
        context_owner.sidebar_presentation.overlay.kind =
            crate::app::state::ClientOverlay::ContextMenu;
        server.clients.insert(2, context_owner);

        assert!(server.promote_client_to_foreground(1));
        assert!(server.promote_client_to_foreground(2));

        assert!(matches!(
            read_server_message(
                control_b
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute input-source state for context-menu owner"),
            ),
            ServerMessage::PrefixInputSource { active: true }
        ));
    }

    #[test]
    fn same_mode_foreground_promotion_sends_absolute_input_source_state() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, control_b, _render_b) = test_client_writer();
        let mut client_a = test_app_client(Some(true), 1);
        client_a.writer = Some(writer_a);
        let mut client_b = test_app_client(Some(true), 2);
        client_b.writer = Some(writer_b);
        server.clients.insert(1, client_a);
        server.clients.insert(2, client_b);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.promote_client_to_foreground(2));

        assert!(matches!(
            read_server_message(
                control_b
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute input-source state for promoted client"),
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));
    }

    #[test]
    fn external_api_pane_focus_sends_absolute_input_source_state_from_prefix() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        server.app.state.set_server_mode(crate::app::Mode::Prefix);
        let workspace = crate::workspace::Workspace::test_new("api-focus");
        let pane_id = workspace.tabs[0].root_pane;
        let pane_number = workspace.public_pane_number(pane_id).expect("pane number");
        let public_pane_id =
            crate::workspace::public_pane_id_for_number(&workspace.id, pane_number);
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        let (writer, control_rx, _render_rx) = test_client_writer();
        let mut client = test_app_client(Some(true), 1);
        client.writer = Some(writer);
        server.clients.insert(1, client);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "focus-from-prefix".into(),
                    method: api::schema::Method::PaneFocus(api::schema::PaneTarget {
                        pane_id: public_pane_id,
                    }),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("pane focus response");
        assert!(
            serde_json::from_str::<api::schema::SuccessResponse>(&response).is_ok(),
            "{response}"
        );
        assert!(matches!(
            read_server_message(
                control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute input-source state after API focus"),
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));
    }

    #[test]
    fn active_overlay_pane_death_sends_absolute_input_source_state_from_prefix() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        let mut workspace = crate::workspace::Workspace::test_new("overlay-exit");
        let previous_focus = workspace.tabs[0].root_pane;
        let overlay_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].zoomed = true;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Prefix);
        server.app.overlay_panes.insert(
            overlay_pane,
            crate::app::OverlayPaneState::test(0, 0, previous_focus, false),
        );
        let (writer, control_rx, _render_rx) = test_client_writer();
        let mut client = test_app_client(Some(true), 1);
        client.writer = Some(writer);
        server.clients.insert(1, client);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::PaneDied {
                pane_id: overlay_pane,
            })
        );

        assert!(matches!(
            read_server_message(
                control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute input-source state after active overlay exit"),
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));
    }

    #[test]
    fn disconnect_promotes_context_menu_owner_and_syncs_its_input_source() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, control_b, _render_b) = test_client_writer();
        let mut client_a = test_app_client(Some(true), 1);
        client_a.writer = Some(writer_a);
        let mut client_b = test_app_client(Some(true), 2);
        client_b.writer = Some(writer_b);
        client_b.sidebar_presentation.overlay.kind = crate::app::state::ClientOverlay::ContextMenu;
        server.clients.insert(1, client_a);
        server.clients.insert(2, client_b);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.remove_client(1));
        assert_eq!(server.foreground_client_id, Some(2));
        assert!(matches!(
            read_server_message(
                control_b
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute input-source state for promoted menu owner"),
            ),
            ServerMessage::PrefixInputSource { active: true }
        ));
    }

    #[test]
    fn queued_input_source_transition_stays_with_its_client_after_repromotion() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        let (writer_a, control_a, _render_a) = test_client_writer();
        let (writer_b, control_b, _render_b) = test_client_writer();
        let mut client_a = test_app_client(Some(true), 1);
        client_a.writer = Some(writer_a);
        client_a.sidebar_presentation.overlay.kind = crate::app::state::ClientOverlay::ContextMenu;
        let mut client_b = test_app_client(Some(true), 2);
        client_b.writer = Some(writer_b);
        server.clients.insert(1, client_a);
        server.clients.insert(2, client_b);
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(server.handle_client_input_events(
            1,
            vec![test_key(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::empty(),
            )],
        ));
        assert!(matches!(
            read_server_message(
                control_a
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute state when original client is promoted"),
            ),
            ServerMessage::PrefixInputSource { active: true }
        ));
        assert!(server.promote_client_to_foreground(2));
        assert!(matches!(
            read_server_message(
                control_b
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute state when replacement client is promoted"),
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));

        let queued = server
            .app
            .event_rx
            .try_recv()
            .expect("queued transition from original client");
        assert!(matches!(
            &queued,
            AppEvent::PrefixInputSource {
                client_id: Some(1),
                active: false,
            }
        ));
        server.handle_internal_event_with_forwarding(queued);
        assert!(matches!(
            read_server_message(
                control_a
                    .recv_timeout(Duration::from_millis(100))
                    .expect("queued transition reaches its original client"),
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));
        assert!(control_b.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn worktree_remove_completion_syncs_owning_clients_input_source() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let checkout = std::path::PathBuf::from("/repo/herdr-issue");
        let mut workspace = crate::workspace::Workspace::test_new("issue");
        workspace.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let workspace_id = workspace.id.clone();
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        let checkout_key = crate::worktree::canonical_or_original(&checkout);
        server
            .app
            .pending_api_worktree_removes
            .insert(workspace_id.clone(), 7);
        server
            .app
            .pending_api_worktree_remove_paths
            .insert(checkout_key.clone(), 7);

        let (writer_a, control_a, _render_a) = test_client_writer();
        let (writer_b, control_b, _render_b) = test_client_writer();
        let mut client_a = test_app_client(Some(true), 1);
        client_a.writer = Some(writer_a);
        client_a.sidebar_presentation.overlay.kind =
            crate::app::state::ClientOverlay::ConfirmRemoveWorktree;
        client_a.sidebar_presentation.overlay.worktree_remove =
            Some(crate::app::state::WorktreeRemoveState {
                workspace_id: workspace_id.clone(),
                repo_root: "/repo/herdr".into(),
                path: checkout.clone(),
                error: None,
                removing: true,
                force_confirmation: false,
            });
        let mut client_b = test_app_client(Some(true), 2);
        client_b.writer = Some(writer_b);
        server.clients.insert(1, client_a);
        server.clients.insert(2, client_b);
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();
        let (respond_to, _response_rx) = std::sync::mpsc::channel();

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::WorktreeRemoveFinished(
                Box::new(crate::events::WorktreeRemoveResult {
                    workspace_id: workspace_id.clone(),
                    path: checkout.clone(),
                    workspace: Some(Box::new(crate::api::schema::WorkspaceInfo {
                        workspace_id: workspace_id.clone(),
                        is_fleet: false,
                        number: 1,
                        label: "issue".into(),
                        focused: true,
                        pane_count: 1,
                        tab_count: 1,
                        active_tab_id: format!("{workspace_id}:t1"),
                        agent_status: crate::api::schema::AgentStatus::Unknown,
                        tokens: Default::default(),
                        worktree: None,
                        repo_binding: None,
                    })),
                    worktree: Some(Box::new(crate::api::schema::WorktreeInfo {
                        path: checkout.display().to_string(),
                        branch: Some("issue".into()),
                        is_bare: false,
                        is_detached: false,
                        is_prunable: false,
                        is_linked_worktree: true,
                        open_workspace_id: Some(workspace_id),
                        label: "issue".into(),
                    })),
                    forced: false,
                    api_request: Some(crate::events::ApiWorktreeRemoveRequest {
                        client_id: Some(1),
                        id: "remove".into(),
                        operation_id: 7,
                        checkout_key,
                        respond_to,
                    }),
                    result: Ok(()),
                })
            ),)
        );

        let queued = server
            .app
            .event_rx
            .try_recv()
            .expect("worktree completion input-source sync");
        assert!(matches!(
            &queued,
            AppEvent::PrefixInputSource {
                client_id: Some(1),
                active: false,
            }
        ));
        server.handle_internal_event_with_forwarding(queued);
        assert!(matches!(
            read_server_message(
                control_a
                    .recv_timeout(Duration::from_millis(100))
                    .expect("worktree owner receives input-source restore"),
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));
        assert!(control_b.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn foreground_client_appearance_controls_auto_theme() {
        let mut server = test_headless_server();
        server.app.state.theme_runtime.auto_switch = true;
        server.app.state.theme_runtime.dark_name = "catppuccin".to_string();
        server.app.state.theme_runtime.light_name = "catppuccin-latte".to_string();
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
                    foreground: None,
                    background: Some(crate::terminal_theme::RgbColor { r: 0, g: 0, b: 0 }),
                    ..Default::default()
                },
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
                    foreground: None,
                    background: Some(crate::terminal_theme::RgbColor {
                        r: 255,
                        g: 255,
                        b: 255,
                    }),
                    ..Default::default()
                },
                None,
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        assert!(server.promote_client_to_foreground(1));
        assert_eq!(server.app.state.theme_name, "catppuccin");

        assert!(server.promote_client_to_foreground(2));
        assert_eq!(server.app.state.theme_name, "catppuccin-latte");
    }

    #[test]
    fn explicit_color_scheme_report_wins_over_osc11_inference() {
        let mut server = test_headless_server();
        server.app.state.theme_runtime.auto_switch = true;
        server.app.state.theme_runtime.dark_name = "catppuccin".to_string();
        server.app.state.theme_runtime.light_name = "catppuccin-latte".to_string();
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: crate::raw_input::GHOSTTY_COLOR_SCHEME_LIGHT_REPORT.to_vec(),
        }));
        assert_eq!(
            server.clients[&1].host_terminal_appearance,
            Some(crate::terminal_theme::HostAppearance::Light)
        );
        assert!(server.clients[&1].host_terminal_appearance_explicit);
        assert_eq!(server.app.state.theme_name, "catppuccin-latte");

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
        }));
        assert_eq!(
            server.clients[&1].host_terminal_appearance,
            Some(crate::terminal_theme::HostAppearance::Light)
        );
        assert!(server.clients[&1].host_terminal_appearance_explicit);
        assert_eq!(server.app.state.theme_name, "catppuccin-latte");
    }

    #[test]
    fn color_scheme_change_event_is_inert_on_server() {
        let mut server = test_headless_server();
        let initial_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x10,
                g: 0x20,
                b: 0x30,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x40,
                g: 0x50,
                b: 0x60,
            }),
            ..Default::default()
        };
        server.app.state.host_terminal_theme = initial_theme;
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                initial_theme,
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: crate::raw_input::GHOSTTY_COLOR_SCHEME_DARK_REPORT.to_vec(),
        });

        assert!(!changed);
        assert_eq!(server.foreground_client_id, None);
        assert_eq!(server.clients[&1].host_terminal_theme, initial_theme);
        assert_eq!(server.app.state.host_terminal_theme, initial_theme);
    }

    #[test]
    fn focus_lost_updates_client_without_promoting_foreground() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[O".to_vec(),
        });

        assert!(!changed);
        assert_eq!(server.foreground_client_id, Some(2));
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(false));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
    }

    #[test]
    fn f2_detached_client_stops_counting_toward_aggregate_focus() {
        let mut server = test_headless_server();
        let (lost_writer, _lost_control_rx, _lost_render_rx) = test_client_writer();
        let (unknown_writer, _unknown_control_rx, _unknown_render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(lost_writer),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(unknown_writer),
            ),
        );

        assert!(server.app_clients_host_focused());
        server.clients.get_mut(&2).expect("client").writer = None;
        assert!(server.clients.contains_key(&2));
        assert!(!server.app_clients_host_focused());
    }

    #[test]
    fn f5_raw_focus_path_holds_work_then_raises_prompt_on_focus_gain() {
        let mut server = test_headless_server();
        let now = Instant::now();
        server.app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            now,
        );
        let (writer, _control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(writer),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[O".to_vec(),
        });
        server.handle_scheduled_tasks_headless(now + Duration::from_secs(60), false);
        assert!(server.app.state.pomodoro.held());
        assert!(server.app.state.pomodoro.prompt.is_none());

        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        });
        let prompt = server
            .app
            .state
            .pomodoro
            .prompt
            .as_ref()
            .expect("focus return raises work reminder");
        assert_eq!(prompt.ended, crate::pomodoro::PomodoroPhase::Work);
        assert!(!server.app.state.pomodoro.held());
    }

    #[test]
    fn f5_raw_focus_path_holds_break_then_starts_focus_on_focus_gain() {
        let mut server = test_headless_server();
        let now = Instant::now();
        server.app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                short_break_minutes: 1,
                ..Default::default()
            },
            now,
        );
        server.app.state.pomodoro.skip(now);
        let (writer, _control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(writer),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[O".to_vec(),
        });
        server.handle_scheduled_tasks_headless(now + Duration::from_secs(60), false);
        assert!(server.app.state.pomodoro.held());

        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        });
        assert_eq!(
            server.app.state.pomodoro.phase,
            crate::pomodoro::PomodoroPhase::Work
        );
        assert!(server.app.state.pomodoro.running());
        assert!(server.app.state.pomodoro.prompt.is_none());
        assert!(!server.app.state.pomodoro.held());
    }

    #[test]
    fn detached_break_expiry_starts_focus_when_an_app_client_attaches() {
        let mut server = test_headless_server();
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        let expired_at = Instant::now() - Duration::from_secs(61);
        server.app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                short_break_minutes: 1,
                ..Default::default()
            },
            expired_at,
        );
        server.app.state.pomodoro.skip(expired_at);
        server.clients.get_mut(&1).expect("client").writer = None;
        assert!(!server.has_app_client());

        server.handle_scheduled_tasks_headless(Instant::now(), false);
        assert!(server.app.state.pomodoro.running());
        assert!(!server.app.state.pomodoro.held());
        assert!(server.app.state.pomodoro.prompt.is_none());
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        assert_eq!(
            server.app.state.pomodoro.phase,
            crate::pomodoro::PomodoroPhase::Work
        );
        assert!(server.app.state.pomodoro.running());
        assert!(server.app.state.pomodoro.prompt.is_none());
        assert!(!server.app.state.pomodoro.held());
    }

    #[test]
    fn detached_focus_expiry_prompts_when_an_app_client_attaches() {
        let mut server = test_headless_server();
        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        server.app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            Instant::now() - Duration::from_secs(61),
        );
        server.clients.get_mut(&1).expect("client").writer = None;
        assert!(!server.has_app_client());
        assert!(server.app.state.pomodoro.prompt.is_none());

        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            direct_graphics: false,
            writer,
        }));

        let prompt = server
            .app
            .state
            .pomodoro
            .prompt
            .as_ref()
            .expect("ended focus phase prompts on attach");
        assert_eq!(prompt.ended, crate::pomodoro::PomodoroPhase::Work);
        assert!(!server.app.state.pomodoro.held());
    }

    #[test]
    fn focus_gained_promotes_client_to_foreground() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        });

        assert!(changed);
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(true));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
    }

    #[tokio::test]
    async fn foreground_focus_gained_reaches_pane_with_focus_reporting() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1004h");

        server.clients.insert(1, test_app_client(Some(false), 1));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));
        assert_eq!(
            input_rx.try_recv().expect("forwarded focus gained report"),
            Bytes::from_static(b"\x1b[I")
        );

        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[O".to_vec(),
        }));
        assert_eq!(
            input_rx.try_recv().expect("forwarded focus lost report"),
            Bytes::from_static(b"\x1b[O")
        );
    }

    #[tokio::test]
    async fn outer_focus_events_do_not_reach_pane_without_focus_reporting() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"");
        server.clients.insert(1, test_app_client(Some(false), 1));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));
        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn background_focus_batch_only_forwards_events_after_promotion() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1004h");
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.clients.insert(2, test_app_client(Some(false), 2));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 2,
            data: b"\x1b[O\x1b[I".to_vec(),
        }));
        assert_eq!(server.foreground_client_id, Some(2));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
        assert_eq!(
            input_rx
                .try_recv()
                .expect("focus gained after client promotion"),
            Bytes::from_static(b"\x1b[I")
        );
        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn background_client_focus_loss_releases_its_owned_keys() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[>15u");
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.clients.insert(2, test_app_client(Some(true), 2));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('j'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,

                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(!server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::FocusLost],
        }));
        assert_eq!(
            input_rx.try_recv().expect("forwarded press"),
            Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            input_rx
                .try_recv()
                .expect("synthetic release from background client"),
            Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(server.app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn structured_outer_focus_events_reach_reporting_pane() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1004h");
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![
                crate::protocol::ClientInputEvent::FocusGained,
                crate::protocol::ClientInputEvent::FocusLost,
            ],
        }));
        assert_eq!(
            input_rx.try_recv().expect("structured focus gained report"),
            Bytes::from_static(b"\x1b[I")
        );
        assert_eq!(
            input_rx.try_recv().expect("structured focus lost report"),
            Bytes::from_static(b"\x1b[O")
        );
    }

    #[tokio::test]
    async fn background_key_makes_later_focus_lost_eligible() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1004h");
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.clients.insert(2, test_app_client(Some(true), 2));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 2,
            events: vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('x'),
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Release,

                    repeat_count: 1,
                    generated_text: None,
                    source: crate::protocol::ClientKeySource::Synthesized,
                },
                crate::protocol::ClientInputEvent::FocusLost,
            ],
        }));
        assert_eq!(server.foreground_client_id, Some(2));
        assert_eq!(
            input_rx.try_recv().expect("focus lost after promotion"),
            Bytes::from_static(b"\x1b[O")
        );
    }

    #[tokio::test]
    async fn pending_terminal_control_drops_structured_input() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1004h");
        server.clients.insert(1, test_app_client(Some(true), 1));

        let mut attached = test_app_client(Some(false), 2);
        attached.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: "attached".to_owned(),
            control: None,
        };
        server.clients.insert(2, attached);

        let mut pending = test_app_client(Some(false), 3);
        pending.pending_terminal_attach = true;
        server.clients.insert(3, pending);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        for client_id in [2, 3] {
            assert!(!server.handle_server_event(ServerEvent::ClientInputEvents {
                client_id,
                events: vec![crate::protocol::ClientInputEvent::FocusGained],
            }));
            assert_eq!(server.foreground_client_id, Some(1));
            assert_eq!(server.app.state.outer_terminal_focus, Some(true));
            assert_eq!(server.clients[&client_id].outer_terminal_focus, Some(false));
        }

        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        assert!(!server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 3,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('x'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Release,

                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));
        assert_eq!(server.foreground_client_id, Some(1));
        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 3,
            data: b"x".to_vec(),
        }));
        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn refused_remote_client_events_cannot_fall_through_to_local_focus_or_draft() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"");
        let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
        let draft = "human draft bytes stay exact: λ🙂".to_owned();
        server
            .app
            .state
            .pending_human_drafts
            .insert(pane_id, draft.clone());
        server.clients.insert(1, test_app_client(Some(true), 1));

        let mut pending = test_app_client(Some(false), 3);
        pending.pending_terminal_attach = true;
        server.clients.insert(3, pending);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(!server.reject_remote_control(
            3,
            api::schema::ErrorBody {
                code: "refused_for_safety".to_owned(),
                message: "test refusal".to_owned(),
            },
        ));
        assert!(!server.clients.contains_key(&3));

        assert!(!server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 3,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('x'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));
        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 3,
            data: b"x".to_vec(),
        }));
        assert!(
            !server.handle_server_event(ServerEvent::ClientClipboardImage {
                client_id: 3,
                extension: "png".to_owned(),
                data: b"not-a-local-paste".to_vec(),
            })
        );
        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(
            server
                .app
                .state
                .pending_human_drafts
                .get(&pane_id)
                .map(String::as_bytes),
            Some(draft.as_bytes())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_owner_excludes_runtime_writers_and_releases_cleanly() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"");
        let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("test runtime");

        assert!(runtime.acquire_remote_owner(7));
        assert!(runtime
            .try_send_bytes(Bytes::from_static(b"api-writer"))
            .is_err());
        assert!(runtime
            .send_bytes(Bytes::from_static(b"async-api-writer"))
            .await
            .is_err());
        assert!(matches!(
            input_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        runtime.release_remote_owner(7);
        runtime
            .try_send_bytes(Bytes::from_static(b"after-release"))
            .expect("runtime writes resume after lease release");
        assert_eq!(
            input_rx.recv().await.expect("released runtime write"),
            Bytes::from_static(b"after-release")
        );
    }

    #[test]
    fn terminal_attach_resize_preserves_known_cell_size_when_pixels_are_omitted() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id, _pane_id| {
            let mut client = ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                },
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            );
            client.mode = ClientConnectionMode::TerminalAttach {
                terminal_id: terminal_id.clone(),
                control: None,
            };
            server.clients.insert(1, client);

            assert!(server.handle_server_event(ServerEvent::ClientResize {
                client_id: 1,
                cols: 100,
                rows: 30,
                cell_width_px: 0,
                cell_height_px: 0,
            }));

            assert_eq!(
                server
                    .runtime_for_terminal_id_string(&terminal_id)
                    .unwrap()
                    .pixel_size(),
                Some((1_000, 600))
            );
            assert_eq!(
                server.clients[&1].cell_size,
                crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                }
            );
        });
    }

    #[test]
    fn controlled_resize_updates_the_controlled_runtime_not_shared_layout() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id, _pane_id| {
            let context = api::schema::RemoteControlContext {
                host: "buildbox".to_owned(),
                user: "operator".to_owned(),
                workspace_id: "w1".to_owned(),
                tab_id: "w1:t1".to_owned(),
                pane_id: "w1:p1".to_owned(),
                terminal_id: terminal_id.clone(),
                cwd: "/work".to_owned(),
                foreground_cwd: "/work".to_owned(),
                tty: "/dev/pts/4".to_owned(),
                foreground_process: api::schema::RemoteForegroundProcess {
                    pid: 1234,
                    process_group_id: 1234,
                    name: "agent".to_owned(),
                    argv: vec!["agent".to_owned()],
                    cwd: "/work".to_owned(),
                },
                detected_agent: "claude".to_owned(),
                interactive_ready: true,
                human_draft: false,
                state_change_seq: 1,
                revision: 1,
                context_epoch: 1,
            };
            let lease = crate::server::remote_control::RemoteControlLease::new(
                api::schema::AgentRef::new("buildbox", "w1:p1").expect("agent ref"),
                context,
            );
            let mut client = test_app_client(Some(false), 7);
            client.mode = ClientConnectionMode::TerminalAttach {
                terminal_id: terminal_id.clone(),
                control: Some(Box::new(lease)),
            };
            server.clients.insert(7, client);
            server.terminal_attach_owners.insert(terminal_id.clone(), 7);

            assert!(server.handle_server_event(ServerEvent::ClientResize {
                client_id: 7,
                cols: 101,
                rows: 31,
                cell_width_px: 10,
                cell_height_px: 20,
            }));
            assert_eq!(
                server
                    .runtime_for_terminal_id_string(&terminal_id)
                    .expect("controlled runtime")
                    .current_size(),
                (31, 101)
            );
            assert_eq!(
                server
                    .runtime_for_terminal_id_string(&terminal_id)
                    .expect("controlled runtime")
                    .pixel_size(),
                Some((1_010, 620))
            );
            assert_eq!(server.clients[&7].terminal_size, (101, 31));
        });
    }

    #[tokio::test]
    async fn passive_mouse_motion_forwards_without_requesting_render() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1003h\x1b[?1006h");
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        let baseline = FrameData {
            cells: Vec::new(),
            width: 0,
            height: 0,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let client = server.clients.get_mut(&1).unwrap();
        let prepared = client
            .render_state
            .prepare_frame(baseline.clone())
            .expect("new semantic baseline");
        client.render_state.commit_sent_frame(prepared);
        let pane = server.app.state.view.pane_infos[0].clone();
        let column = pane.inner_rect.x + 2;
        let row = pane.inner_rect.y + 3;
        let input = format!("\x1b[<35;{};{}M", column + 1, row + 1).into_bytes();

        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: input,
        }));
        assert_eq!(
            input_rx.try_recv().expect("forwarded mouse motion"),
            Bytes::from_static(b"\x1b[<35;3;4M")
        );
        assert_eq!(
            server.clients[&1].render_state.last_frame(),
            Some(&baseline)
        );
    }

    #[test]
    fn background_mouse_motion_promotes_once_then_becomes_render_neutral() {
        let mut server = test_headless_server();
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.clients.insert(2, test_app_client(Some(true), 2));
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        let motion = || ServerEvent::ClientInputEvents {
            client_id: 2,
            events: vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 10,
                row: 5,
                modifiers: 0,
            }],
        };

        assert!(server.handle_server_event(motion()));
        assert_eq!(server.foreground_client_id, Some(2));
        assert!(!server.handle_server_event(motion()));
    }

    #[test]
    fn attached_context_menu_keeps_mouse_motion_rendering_client_local_hover() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("menu")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.clients.insert(2, test_app_client(Some(true), 2));
        server
            .clients
            .get_mut(&2)
            .expect("second client")
            .sidebar_presentation
            .overlay = crate::app::state::ClientOverlayState {
            kind: crate::app::state::ClientOverlay::ContextMenu,
            context_menu: Some(crate::app::state::ContextMenuState {
                kind: crate::app::state::ContextMenuKind::Workspace {
                    workspace_id: server.app.state.workspaces[0].id.clone(),
                    ws_idx: 0,
                },
                x: 4,
                y: 3,
                selected: crate::app::state::ContextMenuAction::RenameWorkspace,
            }),
            ..Default::default()
        };
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        let motion = || ServerEvent::ClientInputEvents {
            client_id: 2,
            events: vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 10,
                row: 5,
                modifiers: 0,
            }],
        };

        assert!(server.handle_server_event(motion()));
        assert_eq!(server.foreground_client_id, Some(2));
        assert!(server.handle_server_event(motion()));
    }

    #[test]
    fn attached_snooze_dropdown_hover_requests_an_immediate_redraw() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("snooze")];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let target = crate::app::state::PaneFocusTarget {
            workspace_id: server.app.state.workspaces[0].id.clone(),
            pane_id: server.app.state.workspaces[0].tabs[0].root_pane,
        };
        server.clients.insert(1, test_app_client(Some(true), 1));
        server.clients.insert(2, test_app_client(Some(true), 2));
        server
            .clients
            .get_mut(&2)
            .expect("second client")
            .sidebar_presentation
            .overlay
            .snooze = Some(crate::app::state::SidebarSnoozeUiState {
            target: crate::app::state::SidebarPaneLifecycleTarget::Local(target),
            anchor: (4, 3),
            selected: crate::app::state::SidebarSnoozeMenuAction::SetTime,
            time_draft: None,
            error: None,
        });
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        let motion = || ServerEvent::ClientInputEvents {
            client_id: 2,
            events: vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 5,
                row: 4,
                modifiers: 0,
            }],
        };

        assert!(server.handle_server_event(motion()));
        assert_eq!(server.foreground_client_id, Some(2));
        assert!(server.handle_server_event(motion()));
    }

    #[test]
    fn attached_group_menu_hover_requests_an_immediate_redraw() {
        let mut server = test_headless_server();
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.clients.insert(1, test_app_client(Some(true), 1));
        let mut menu_client = test_app_client(Some(true), 2);
        menu_client.sidebar_presentation.group_menu_open = true;
        server.clients.insert(2, menu_client);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        let motion = || ServerEvent::ClientInputEvents {
            client_id: 2,
            events: vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 10,
                row: 5,
                modifiers: 0,
            }],
        };

        assert!(server.handle_server_event(motion()));
        assert_eq!(server.foreground_client_id, Some(2));
        assert!(server.handle_server_event(motion()));
    }

    #[test]
    fn mouse_motion_in_hover_modes_requires_render() {
        let events = [crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::Moved,
                column: 10,
                row: 5,
                modifiers: KeyModifiers::empty(),
            },
        )];

        assert!(events_are_render_neutral_mouse_motion(
            &events,
            crate::app::Mode::Terminal
        ));
        for mode in [
            crate::app::Mode::GlobalMenu,
            crate::app::Mode::ContextMenu,
            crate::app::Mode::Navigator,
        ] {
            assert!(!events_are_render_neutral_mouse_motion(&events, mode));
        }
    }

    fn install_focused_test_runtime(
        server: &mut HeadlessServer,
        terminal_bytes: &[u8],
    ) -> tokio::sync::mpsc::Receiver<Bytes> {
        let mut workspace = crate::workspace::Workspace::test_new("focus-reporting");
        let pane_id = workspace.tabs[0].root_pane;
        let (runtime, input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                terminal_bytes,
                4,
            );
        workspace.insert_test_runtime(pane_id, runtime);
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        input_rx
    }

    fn test_app_client(outer_terminal_focus: Option<bool>, last_activity: u64) -> ClientConnection {
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            crate::terminal_theme::TerminalTheme::default(),
            outer_terminal_focus,
            last_activity,
            RenderEncoding::SemanticFrame,
            None,
        )
    }

    fn test_key(
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> crate::raw_input::RawInputEvent {
        crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(code, modifiers))
    }

    #[tokio::test]
    async fn clipboard_image_with_client_overlay_never_reaches_pane() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"");
        let mut client = test_app_client(Some(true), 1);
        client.sidebar_presentation.overlay.kind = crate::app::state::ClientOverlay::ConfirmClose;
        client
            .sidebar_presentation
            .overlay
            .confirm_close_workspace_id = Some(server.app.state.workspaces[0].id.clone());
        server.clients.insert(1, client);

        assert!(
            server.handle_server_event(ServerEvent::ClientClipboardImage {
                client_id: 1,
                extension: "png".into(),
                data: b"not-a-real-image".to_vec(),
            })
        );

        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pixel_mouse_with_client_overlay_never_reaches_pane() {
        let mut server = test_headless_server();
        let mut input_rx = install_focused_test_runtime(&mut server, b"\x1b[?1000h\x1b[?1016h");
        let pane_id = server.app.state.workspaces[0].tabs[0].root_pane;
        server.app.pane_graphics.slots.insert(
            (pane_id, "test".into()),
            crate::app::pane_graphics::Slot::test(
                1,
                Some(crate::app::pane_graphics::Layer {
                    format: crate::api::schema::PaneGraphicsFormat::Rgba,
                    image_width: 1,
                    image_height: 1,
                    backing: crate::app::pane_graphics::Backing::Inline(vec![0; 4]),
                    data_fingerprint: 0,
                    render: crate::api::schema::PaneGraphicsPlacementParams::default(),
                    z_index: 0,
                }),
            ),
        );
        let mut client = test_app_client(Some(true), 1);
        client.cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        client.host_sgr_pixels_active = Some(true);
        client.sidebar_presentation.overlay.kind = crate::app::state::ClientOverlay::ConfirmClose;
        client
            .sidebar_presentation
            .overlay
            .confirm_close_workspace_id = Some(server.app.state.workspaces[0].id.clone());
        server.clients.insert(1, client);
        let geometry =
            crate::input::mouse::HostGeometry::new(80, 24, 800, 480).expect("host geometry");

        assert!(server.handle_server_event(ServerEvent::ClientInputPixels {
            client_id: 1,
            data: b"\x1b[<0;10;10M".to_vec(),
            geometry,
        }));

        assert!(input_rx.try_recv().is_err());
    }

    #[test]
    fn attached_focus_changing_pane_action_preserves_shared_mode_and_draft() {
        for server_mode in [
            crate::app::Mode::Settings,
            crate::app::Mode::GlobalMenu,
            crate::app::Mode::AddAction,
        ] {
            let mut server = test_headless_server();
            let mut workspace = crate::workspace::Workspace::test_new("mode");
            let source_pane_id = workspace.tabs[0].root_pane;
            let pane_id = workspace.test_split(ratatui::layout::Direction::Horizontal);
            workspace.tabs[0].layout.focus_pane(source_pane_id);
            server.app.state.workspaces = vec![workspace];
            server.app.state.ensure_test_terminals();
            server.app.state.active = Some(0);
            server.app.state.selected = 0;
            server.app.state.set_server_mode(server_mode);
            server.app.state.settings.search = "keep settings draft".into();
            server.app.state.global_menu.highlighted = 2;
            server.app.state.add_action = Some(crate::app::state::AddActionState {
                name: "keep action draft".into(),
                command: "cargo test".into(),
                ..Default::default()
            });
            let workspace_id = server.app.state.workspaces[0].id.clone();
            let tab_id = crate::workspace::public_tab_id_for_number(
                &workspace_id,
                server.app.state.workspaces[0].tabs[0].number,
            );
            let mut client_a = test_app_client(Some(true), 1);
            client_a.sidebar_presentation.overlay = crate::app::state::ClientOverlayState {
                kind: crate::app::state::ClientOverlay::ContextMenu,
                context_menu: Some(crate::app::state::ContextMenuState {
                    kind: crate::app::state::ContextMenuKind::Pane {
                        workspace_id,
                        tab_id,
                        ws_idx: 0,
                        tab_idx: 0,
                        pane_id,
                        source_pane_id: Some(source_pane_id),
                        has_manual_label: false,
                        right_click_passthrough: false,
                        linkable_work_link: None,
                        link: None,
                        path: None,
                        open_with: Vec::new(),
                        send_text: None,
                        has_agent_targets: false,
                    },
                    x: 2,
                    y: 2,
                    selected: crate::app::state::ContextMenuAction::Zoom,
                }),
                ..Default::default()
            };
            server.clients.insert(1, client_a);
            server.clients.insert(2, test_app_client(Some(true), 2));
            server.foreground_client_id = Some(2);
            server.sync_foreground_client_state();

            assert!(server.handle_client_input_events(
                1,
                vec![test_key(
                    crossterm::event::KeyCode::Enter,
                    crossterm::event::KeyModifiers::empty(),
                )],
            ));

            assert_eq!(server.app.state.server_mode(), server_mode);
            assert_eq!(server.app.state.settings.search, "keep settings draft");
            assert_eq!(server.app.state.global_menu.highlighted, 2);
            let add_action = server
                .app
                .state
                .add_action
                .as_ref()
                .expect("add-action draft");
            assert_eq!(add_action.name, "keep action draft");
            assert_eq!(add_action.command, "cargo test");
            assert_eq!(
                server.app.state.workspaces[0].focused_pane_id(),
                Some(pane_id)
            );
            assert!(server.app.state.workspaces[0].tabs[0].zoomed);
            assert_eq!(
                server.clients[&1].sidebar_presentation.overlay.kind,
                crate::app::state::ClientOverlay::None
            );
            assert_eq!(
                server.clients[&1].sidebar_presentation.focus_intent,
                crate::app::state::ClientFocusIntent::Pane
            );
            assert_eq!(
                server.clients[&2].sidebar_presentation.focus_intent,
                crate::app::state::ClientFocusIntent::FollowShared
            );
        }
    }

    #[test]
    fn attached_mouse_context_action_preserves_other_clients_settings() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("mouse-mode");
        let source_pane_id = workspace.tabs[0].root_pane;
        let pane_id = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(source_pane_id);
        let workspace_id = workspace.id.clone();
        let tab_id =
            crate::workspace::public_tab_id_for_number(&workspace_id, workspace.tabs[0].number);
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Settings);
        let context_menu = crate::app::state::ContextMenuState {
            kind: crate::app::state::ContextMenuKind::Pane {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                pane_id,
                source_pane_id: Some(source_pane_id),
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
                link: None,
                path: None,
                open_with: Vec::new(),
                send_text: None,
                has_agent_targets: false,
            },
            x: 2,
            y: 2,
            selected: crate::app::state::ContextMenuAction::Zoom,
        };
        let zoom_row = context_menu
            .items()
            .iter()
            .position(|item| *item == "Zoom")
            .expect("zoom item") as u16;
        let mut client_a = test_app_client(Some(true), 1);
        client_a.sidebar_presentation.overlay = crate::app::state::ClientOverlayState {
            kind: crate::app::state::ClientOverlay::ContextMenu,
            context_menu: Some(context_menu),
            ..Default::default()
        };
        server.clients.insert(1, client_a);
        server.clients.insert(2, test_app_client(Some(true), 2));
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: 3,
                    row: 3 + zoom_row,
                    modifiers: crossterm::event::KeyModifiers::empty(),
                },
            )],
        ));

        assert_eq!(server.app.state.server_mode(), crate::app::Mode::Settings);
        assert!(server.app.state.workspaces[0].tabs[0].zoomed);
        assert_eq!(
            server.clients[&1].sidebar_presentation.overlay.kind,
            crate::app::state::ClientOverlay::None
        );
        assert_eq!(
            server.clients[&1].sidebar_presentation.focus_intent,
            crate::app::state::ClientFocusIntent::Pane
        );
        assert_eq!(
            server.clients[&2].sidebar_presentation.focus_intent,
            crate::app::state::ClientFocusIntent::FollowShared
        );
    }

    #[test]
    fn attached_agent_picker_stays_with_its_client_and_preserves_settings() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("send");
        workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.test_split(ratatui::layout::Direction::Horizontal);
        let workspace_id = workspace.id.clone();
        let tab_id =
            crate::workspace::public_tab_id_for_number(&workspace_id, workspace.tabs[0].number);
        let panes = workspace.tabs[0].panes.keys().copied().collect::<Vec<_>>();
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        for pane_id in &panes {
            let terminal_id = server.app.state.workspaces[0]
                .terminal_id(*pane_id)
                .expect("pane terminal")
                .clone();
            server
                .app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .detected_agent = Some(crate::detect::Agent::Claude);
        }
        server.app.state.set_server_mode(crate::app::Mode::Settings);

        let source_pane = panes[0];
        let mut client_a = test_app_client(Some(true), 1);
        client_a.sidebar_presentation.overlay = crate::app::state::ClientOverlayState {
            kind: crate::app::state::ClientOverlay::ContextMenu,
            context_menu: Some(crate::app::state::ContextMenuState {
                kind: crate::app::state::ContextMenuKind::Pane {
                    workspace_id,
                    tab_id,
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: source_pane,
                    source_pane_id: Some(source_pane),
                    has_manual_label: false,
                    right_click_passthrough: false,
                    linkable_work_link: None,
                    link: None,
                    path: None,
                    open_with: Vec::new(),
                    send_text: Some("review this".into()),
                    has_agent_targets: true,
                },
                x: 2,
                y: 2,
                selected: crate::app::state::ContextMenuAction::SendToExistingAgent,
            }),
            ..Default::default()
        };
        server.clients.insert(1, client_a);
        server.clients.insert(2, test_app_client(Some(true), 2));
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(server.handle_client_input_events(
            1,
            vec![test_key(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            )],
        ));
        assert_eq!(server.app.state.server_mode(), crate::app::Mode::Settings);
        assert!(server.clients[&1]
            .sidebar_presentation
            .overlay
            .agent_picker
            .is_some());
        assert!(server.clients[&2]
            .sidebar_presentation
            .overlay
            .agent_picker
            .is_none());

        let picker_len = server.clients[&1]
            .sidebar_presentation
            .overlay
            .agent_picker
            .as_ref()
            .expect("client A picker")
            .candidates
            .len();
        let popup = crate::ui::centered_popup_rect(
            ratatui::layout::Rect::new(0, 0, 80, 24),
            78,
            (picker_len as u16).saturating_add(6),
        )
        .expect("picker popup");
        let inner =
            ratatui::layout::Rect::new(popup.x + 1, popup.y + 1, popup.width - 2, popup.height - 2);
        let content = crate::ui::modal_stack_areas(inner, 1, 1, 0, 1).content;
        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: content.x,
                    row: content.y + 1,
                    modifiers: crossterm::event::KeyModifiers::empty(),
                },
            )],
        ));
        assert_eq!(server.app.state.server_mode(), crate::app::Mode::Settings);
        assert!(server.clients[&1]
            .sidebar_presentation
            .overlay
            .agent_picker
            .is_none());
    }

    #[tokio::test]
    async fn attached_settled_focus_and_delete_preserve_other_clients_settings() {
        for selected in [0, 3] {
            let mut server = test_headless_server();
            let workspace = crate::workspace::Workspace::test_new("settled");
            let target = crate::app::state::PaneFocusTarget {
                workspace_id: workspace.id.clone(),
                pane_id: workspace.tabs[0].root_pane,
            };
            server.app.state.workspaces = vec![workspace];
            server.app.state.ensure_test_terminals();
            server.app.state.active = Some(0);
            server.app.state.selected = 0;
            server.app.state.confirm_close = false;
            assert!(server
                .app
                .state
                .settle_pane_at(0, target.pane_id, 1_725_000_000));
            server.app.state.set_server_mode(crate::app::Mode::Settings);

            let mut client_a = test_app_client(Some(true), 1);
            client_a.sidebar_presentation.overlay.settled_menu_target = Some(target);
            client_a.sidebar_presentation.overlay.settled_menu_selected = selected;
            server.clients.insert(1, client_a);
            server.clients.insert(2, test_app_client(Some(true), 2));
            server.foreground_client_id = Some(2);
            server.sync_foreground_client_state();

            assert!(server.handle_client_input_events(
                1,
                vec![test_key(
                    crossterm::event::KeyCode::Enter,
                    crossterm::event::KeyModifiers::empty(),
                )],
            ));
            assert_eq!(server.app.state.server_mode(), crate::app::Mode::Settings);
            assert!(server.clients[&1]
                .sidebar_presentation
                .overlay
                .settled_menu_target
                .is_none());
            assert_eq!(
                server.clients[&1].sidebar_presentation.focus_intent,
                if selected == 0 {
                    crate::app::state::ClientFocusIntent::Pane
                } else {
                    crate::app::state::ClientFocusIntent::FollowShared
                }
            );
            assert_eq!(
                server.clients[&2].sidebar_presentation.focus_intent,
                crate::app::state::ClientFocusIntent::FollowShared
            );
            shutdown_test_runtimes(&mut server);
        }
    }

    #[test]
    fn attached_rename_overlay_takes_keys_before_an_open_dock_surface_menu() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("rename")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let workspace_id = server.app.state.workspaces[0].id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace_id,
            server.app.state.workspaces[0].tabs[0].number,
        );
        let mut client = test_app_client(Some(true), 1);
        client.sidebar_presentation.overlay = crate::app::state::ClientOverlayState {
            kind: crate::app::state::ClientOverlay::RenameTab,
            rename_target: Some(crate::app::state::RenameTarget::Tab {
                workspace_id,
                tab_id,
            }),
            name_input_replace_on_type: false,
            ..Default::default()
        };
        client.dock_presentation.collapsed = false;
        client.dock_presentation.tab = Some(crate::app::DockSurface::Files);
        client.dock_presentation.files_focused = true;
        client.dock_presentation.surface_menu =
            Some(crate::app::state::DockSurfaceMenu { selected: 0 });
        server.clients.insert(1, client);

        assert!(server.handle_client_input_events(
            1,
            vec![test_key(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::empty(),
            )],
        ));

        assert_eq!(
            server.clients[&1].sidebar_presentation.overlay.name_input,
            "x"
        );
        assert!(server.clients[&1].dock_presentation.files_filter.is_empty());
        assert_eq!(
            server.clients[&1].dock_presentation.surface_menu,
            Some(crate::app::state::DockSurfaceMenu { selected: 0 })
        );
    }

    #[test]
    fn attached_client_cannot_confirm_another_clients_pr_action() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("pr")];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let confirmation = crate::app::state::PrActionConfirmation {
            key: crate::app::state::WorkItemKey {
                repo: "owner/repo".into(),
                pr_number: Some(42),
                pr_url: Some("https://github.com/owner/repo/pull/42".into()),
                ticket_id: None,
            },
            action: crate::ui::work_list_detail::PrActionKind::Merge(
                crate::config::MergeMethodConfig::Rebase,
            ),
        };
        let mut client_a = test_app_client(Some(true), 1);
        client_a.dock_presentation.pr_action_confirmation = Some(confirmation.clone());
        server.clients.insert(1, client_a);
        server.clients.insert(2, test_app_client(Some(true), 2));
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(server.handle_client_input_events(
            2,
            vec![test_key(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            )],
        ));

        assert!(server.app.state.request_pr_command.is_none());
        assert_eq!(
            server.clients[&1].dock_presentation.pr_action_confirmation,
            Some(confirmation)
        );
    }

    #[test]
    fn attached_clients_keep_dock_drafts_and_pending_writes_isolated() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("dock")];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let pending = crate::work_index::WorkItemWrite::ApprovePullRequest {
            repo: "owner/repo".into(),
            number: 42,
        };
        let mut client_a = test_app_client(Some(true), 1);
        client_a.dock_presentation.collapsed = false;
        client_a.dock_presentation.tab = Some(crate::app::DockSurface::Home);
        client_a.dock_presentation.home_focused = true;
        client_a.dock_presentation.comment_draft = Some("client A draft".into());
        client_a.dock_presentation.pending_write = Some(pending.clone());
        client_a.dock_presentation.write_notice = Some("client A notice".into());
        let mut client_b = test_app_client(Some(true), 2);
        client_b.dock_presentation.collapsed = false;
        client_b.dock_presentation.tab = Some(crate::app::DockSurface::Home);
        client_b.dock_presentation.home_focused = true;
        server.clients.insert(1, client_a);
        server.clients.insert(2, client_b);

        assert!(server.handle_client_input_events(
            2,
            ['r', 'x', '\u{1b}', 'y']
                .into_iter()
                .map(|character| {
                    test_key(
                        if character == '\u{1b}' {
                            crossterm::event::KeyCode::Esc
                        } else {
                            crossterm::event::KeyCode::Char(character)
                        },
                        crossterm::event::KeyModifiers::empty(),
                    )
                })
                .collect(),
        ));

        let client_a = &server.clients[&1].dock_presentation;
        assert_eq!(client_a.comment_draft.as_deref(), Some("client A draft"));
        assert_eq!(client_a.pending_write, Some(pending));
        assert_eq!(client_a.write_notice.as_deref(), Some("client A notice"));
        let client_b = &server.clients[&2].dock_presentation;
        assert!(client_b.comment_draft.is_none());
        assert!(client_b.pending_write.is_none());
    }

    #[test]
    fn render_is_input_neutral_and_foreground_transition_reconciles_owner() {
        let mut server = test_headless_server();
        server.app.state.switch_ascii_input_source_in_prefix = true;
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("menu")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let workspace_id = server.app.state.workspaces[0].id.clone();
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, _control_b, _render_b) = test_client_writer();
        let mut client_a = test_app_client(Some(true), 1);
        client_a.writer = Some(writer_a);
        client_a.sidebar_presentation.overlay = crate::app::state::ClientOverlayState {
            kind: crate::app::state::ClientOverlay::ContextMenu,
            context_menu: Some(crate::app::state::ContextMenuState {
                kind: crate::app::state::ContextMenuKind::Workspace {
                    workspace_id,
                    ws_idx: 0,
                },
                x: 4,
                y: 3,
                selected: crate::app::state::ContextMenuAction::RenameWorkspace,
            }),
            ..Default::default()
        };
        let mut client_b = test_app_client(Some(true), 2);
        client_b.writer = Some(writer_b);
        server.clients.insert(1, client_a);
        server.clients.insert(2, client_b);
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("replacement")];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;

        server.render_and_stream();

        assert_eq!(
            server.clients[&1].sidebar_presentation.overlay.kind,
            crate::app::state::ClientOverlay::ContextMenu,
            "render changed a background client's input owner"
        );
        assert!(server.handle_client_input_events(
            1,
            vec![test_key(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::empty(),
            )],
        ));
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(
            server.clients[&1].sidebar_presentation.overlay.kind,
            crate::app::state::ClientOverlay::None
        );
        let mut restored = false;
        while let Ok(event) = server.app.event_rx.try_recv() {
            if matches!(
                event,
                crate::events::AppEvent::PrefixInputSource { active: false, .. }
            ) {
                restored = true;
            }
        }
        assert!(
            restored,
            "foreground transition did not restore input source"
        );
    }

    #[test]
    fn fleet_refresh_clears_every_clients_missing_remote_selection() {
        let mut server = test_headless_server();
        let selected =
            crate::api::schema::AgentRef::new("remote", "worker").expect("valid remote selection");
        for client_id in [1, 2] {
            let mut client = test_app_client(Some(true), client_id);
            client.sidebar_presentation.selected_remote_agent = Some(selected.clone());
            server.clients.insert(client_id, client);
        }

        assert!(server.handle_internal_event_with_forwarding(
            crate::events::AppEvent::FleetRefreshed {
                snapshot: crate::fleet::Snapshot::default(),
            },
        ));

        for client_id in [1, 2] {
            assert!(server.clients[&client_id]
                .sidebar_presentation
                .selected_remote_agent
                .is_none());
        }
    }

    #[test]
    fn attached_clients_keep_sidebar_snooze_editor_and_actions_isolated() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("isolation");
        workspace.test_add_tab(Some("client-b"));
        let editor_pane = workspace.tabs[0].root_pane;
        let client_pane = workspace.tabs[1].root_pane;
        let workspace_id = workspace.id.clone();
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let mut client_a = test_app_client(Some(true), 1);
        client_a.sidebar_presentation.focused = true;
        client_a.sidebar_presentation.overlay.snooze =
            Some(crate::app::state::SidebarSnoozeUiState {
                target: crate::app::state::SidebarPaneLifecycleTarget::Local(
                    crate::app::state::PaneFocusTarget {
                        workspace_id,
                        pane_id: editor_pane,
                    },
                ),
                anchor: (2, 2),
                selected: crate::app::state::SidebarSnoozeMenuAction::SetTime,
                time_draft: Some(String::new()),
                error: Some("client A only".into()),
            });
        let mut client_b = test_app_client(Some(true), 2);
        client_b.sidebar_presentation.focused = true;
        server.clients.insert(1, client_a);
        server.clients.insert(2, client_b);

        assert!(server.handle_client_input_events(
            1,
            "14:30"
                .chars()
                .map(|ch| {
                    test_key(
                        crossterm::event::KeyCode::Char(ch),
                        crossterm::event::KeyModifiers::empty(),
                    )
                })
                .collect(),
        ));
        server
            .clients
            .get_mut(&1)
            .expect("client A")
            .sidebar_presentation
            .overlay
            .snooze
            .as_mut()
            .expect("client A editor")
            .error = Some("client A only".into());
        server.app.state.workspaces[0].active_tab = 1;
        assert!(server.handle_client_input_events(
            2,
            ['z', '\n', 'z', '\n', 's']
                .into_iter()
                .map(|ch| {
                    test_key(
                        if ch == '\n' {
                            crossterm::event::KeyCode::Enter
                        } else {
                            crossterm::event::KeyCode::Char(ch)
                        },
                        crossterm::event::KeyModifiers::empty(),
                    )
                })
                .collect(),
        ));

        assert!(server.app.state.pane_is_settled(0, client_pane));
        assert!(!server.app.state.pane_is_settled(0, editor_pane));
        assert!(!server.app.state.pane_is_snoozed(0, editor_pane));
        let editor = server.clients[&1]
            .sidebar_presentation
            .overlay
            .snooze
            .as_ref()
            .expect("client A editor");
        assert_eq!(
            editor.target.local().map(|target| target.pane_id),
            Some(editor_pane)
        );
        assert_eq!(editor.time_draft.as_deref(), Some("14:30"));
        assert_eq!(editor.error.as_deref(), Some("client A only"));
    }

    #[test]
    fn foreground_client_focus_event_updates_app_focus_state() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[O".to_vec(),
        });

        assert!(!changed);
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(false));
        assert_eq!(server.app.state.outer_terminal_focus, Some(false));
    }

    #[test]
    fn app_client_lone_escape_closes_navigate_mode() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Navigate);
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b".to_vec(),
        }));

        assert_eq!(server.app.state.server_mode(), crate::app::Mode::Terminal);
    }

    #[test]
    fn semantic_client_input_events_route_through_app_input() {
        let mut server = test_headless_server();
        server
            .app
            .state
            .set_server_mode(crate::app::Mode::Onboarding);
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Enter,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,

                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));

        assert_eq!(server.app.state.server_mode(), crate::app::Mode::Settings);
        assert_eq!(
            server.app.state.settings.section,
            crate::app::state::SettingsSection::Integrations
        );
    }

    #[tokio::test]
    async fn raw_headless_dock_navigation_intercepts_arrows_but_forwards_pane_input() {
        let mut server = test_headless_server();
        server.app.state.workspaces = [10_u64, 20]
            .into_iter()
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        let focused_pane = server.app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 2);
        server.app.state.workspaces[0].insert_test_runtime(focused_pane, runtime);
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.ensure_test_terminals();
        for (ws_idx, number) in [10_u64, 20].into_iter().enumerate() {
            let pane_id = server.app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = server.app.state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .expect("root terminal")
                .clone();
            server
                .app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("valid work context");
        }
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let first_key = server.app.state.dock_home_projection().rows[0].key.clone();

        let mut client = test_app_client(Some(true), 1);
        client.dock_presentation.collapsed = false;
        client.dock_presentation.tab = Some(crate::app::DockSurface::Home);
        client.dock_presentation.home_focused = true;
        client.dock_presentation.home_selection = Some(first_key);
        server.clients.insert(1, client);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[B".to_vec(),
        }));
        assert_eq!(
            server.clients[&1]
                .dock_presentation
                .home_selection
                .as_ref()
                .and_then(|key| key.pr_number),
            Some(20)
        );
        assert!(input_rx.try_recv().is_err());

        let selection = server.clients[&1].dock_presentation.home_selection.clone();
        let _ = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"j".to_vec(),
        });
        assert_eq!(
            server.clients[&1].dock_presentation.home_selection,
            selection
        );
        assert_eq!(
            input_rx
                .try_recv()
                .expect("bare letter reaches focused pane"),
            Bytes::from_static(b"j")
        );

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![
                crate::protocol::ClientInputEvent::TextCommit("λ".into()),
                crate::protocol::ClientInputEvent::Paste {
                    text: "bulk".into(),
                },
            ],
        }));
        assert_eq!(
            input_rx.try_recv().expect("committed text reaches pane"),
            Bytes::from_static("λ".as_bytes())
        );
        assert_eq!(
            input_rx.try_recv().expect("paste reaches pane"),
            Bytes::from_static(b"bulk")
        );
    }

    #[test]
    fn raw_headless_input_workspace_focus_follows_bound_pr_in_its_dock_presentation() {
        let mut server = test_headless_server();
        server.app.state.workspaces = [10_u64, 20]
            .into_iter()
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.ensure_test_terminals();
        for (ws_idx, number) in [10_u64, 20].into_iter().enumerate() {
            let pane_id = server.app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = server.app.state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .expect("root terminal")
                .clone();
            server
                .app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("valid work context");
        }
        server.app.state.set_server_mode(crate::app::Mode::Navigate);
        let first_key = server.app.state.dock_home_projection().rows[0].key.clone();

        let mut client = ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            crate::terminal_theme::TerminalTheme::default(),
            Some(true),
            1,
            RenderEncoding::SemanticFrame,
            None,
        );
        client.dock_presentation.home_selection = Some(first_key);
        client.dock_presentation.home_section = crate::app::state::DockHomeSection::Tickets;
        server.clients.insert(1, client);
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"2".to_vec(),
        }));

        assert_eq!(server.app.state.active, Some(1));
        let presentation = &server.clients[&1].dock_presentation;
        assert_eq!(
            presentation
                .home_selection
                .as_ref()
                .and_then(|key| key.pr_url.as_deref()),
            Some("https://github.com/owner/repo/pull/20")
        );
        assert_eq!(
            presentation.home_section,
            crate::app::state::DockHomeSection::Prs
        );
        assert_eq!(
            presentation
                .home_followed_pane
                .as_ref()
                .map(|target| target.pane_id),
            Some(server.app.state.workspaces[1].tabs[0].root_pane)
        );
    }

    #[test]
    fn semantic_client_escape_closes_keybind_help() {
        let mut server = test_headless_server();
        server
            .app
            .state
            .set_server_mode(crate::app::Mode::KeybindHelp);
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,

                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));

        assert_eq!(server.app.state.server_mode(), crate::app::Mode::Navigate);
    }

    #[test]
    fn semantic_client_down_scrolls_keybind_help() {
        let mut server = test_headless_server();
        server
            .app
            .state
            .set_server_mode(crate::app::Mode::KeybindHelp);
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        assert!(server.app.state.keybind_help_max_scroll() > 0);
        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Down,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,

                repeat_count: 1,
                generated_text: None,
                source: crate::protocol::ClientKeySource::Synthesized,
            }],
        }));

        assert_eq!(
            server.app.state.server_mode(),
            crate::app::Mode::KeybindHelp
        );
        assert_eq!(server.app.state.keybind_help.scroll, 1);
    }

    #[tokio::test]
    async fn split_default_background_response_updates_theme_without_forwarding_tail() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let _ = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b]".to_vec(),
        });
        assert!(rx.try_recv().is_err());

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"11;#123456\x07".to_vec(),
        }));

        assert!(rx.try_recv().is_err());
        assert_eq!(
            server.clients[&1].host_terminal_theme.background,
            Some(crate::terminal_theme::RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            })
        );
        assert_eq!(
            server.app.state.host_terminal_theme.background,
            Some(crate::terminal_theme::RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            })
        );
    }

    #[tokio::test]
    async fn render_and_stream_uses_each_client_terminal_size() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let active_pane = workspace.tabs[0].root_pane;
        let background_tab = workspace.test_add_tab(Some("background"));
        let background_pane = workspace.tabs[background_tab].root_pane;
        workspace.tabs[0].runtimes.insert(
            active_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"active"),
        );
        workspace.tabs[background_tab].runtimes.insert(
            background_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"background"),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let (desktop_tx, _desktop_control_rx, desktop_rx) = test_client_writer();
        let (mobile_tx, _mobile_control_rx, mobile_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(desktop_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (44, 20),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(mobile_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        server.render_and_stream();

        let desktop_frame = read_server_frame(desktop_rx.recv().expect("desktop frame"));
        let mobile_frame = read_server_frame(mobile_rx.recv().expect("mobile frame"));

        assert_eq!((desktop_frame.width, desktop_frame.height), (120, 40));
        assert_eq!((mobile_frame.width, mobile_frame.height), (44, 20));
        let mobile_text = frame_text(&mobile_frame);
        let mut mobile_rows = mobile_text.lines();
        let mobile_header = mobile_rows.by_ref().take(2).collect::<String>();
        let mobile_surface = mobile_rows.collect::<String>();
        assert!(mobile_header.contains("test"), "header: {mobile_header:?}");
        assert!(
            mobile_surface.contains("active"),
            "surface: {mobile_surface:?}"
        );
        assert!(!mobile_surface.contains("background"));

        // Full-width status bar occupies row 0. The tab row is hidden by default,
        // so the terminal surface starts at y=1 and keeps the row for itself.
        let foreground_terminal_area = Rect::new(26, 1, 93, 39);
        let expected_pane_size = (
            foreground_terminal_area.height,
            foreground_terminal_area.width.saturating_sub(1),
        );
        assert_eq!(
            server.app.state.view.layout,
            crate::app::state::ViewLayout::Desktop
        );
        assert_eq!(server.app.state.view.mobile_header_rect, Rect::default());
        assert_eq!(
            server.app.state.view.terminal_area,
            foreground_terminal_area
        );
        assert_eq!(
            server.app.state.workspaces[0].tabs[0].runtimes[&active_pane].current_size(),
            expected_pane_size
        );
        assert_eq!(
            server.app.state.workspaces[0].tabs[background_tab].runtimes[&background_pane]
                .current_size(),
            expected_pane_size
        );
    }

    #[tokio::test]
    async fn f1_send_off_input_visibility_is_scoped_to_the_source_client() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"\x1b[?1003h\x1b[?1006h");
        let (wide_tx, _wide_control_rx, wide_rx) = test_client_writer();
        let (compact_tx, _compact_control_rx, compact_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(wide_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (18, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(compact_tx),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        let raised_at = Instant::now();
        server.app.state.pomodoro.enabled = true;
        server.app.state.pomodoro.prompt = Some(crate::pomodoro::PomodoroPrompt {
            ended: crate::pomodoro::PomodoroPhase::Work,
            next: crate::pomodoro::PomodoroPhase::ShortBreak,
            raised_at,
            input: String::new(),
            error: None,
        });
        assert_eq!(server.app.state.screen_rect(), Rect::new(0, 0, 18, 30));
        assert_eq!(
            server.pomodoro_animation_deadline(raised_at),
            Some(raised_at + crate::ui::pomodoro::ANIMATION_FRAME_INTERVAL)
        );
        assert!(server
            .pomodoro_animation_due(raised_at + crate::ui::pomodoro::ANIMATION_FRAME_INTERVAL));
        assert!(server.handle_scheduled_tasks_headless(
            raised_at + crate::ui::pomodoro::ANIMATION_FRAME_INTERVAL,
            false,
        ));
        server.render_and_stream();

        let wide_frame = read_server_frame(wide_rx.recv().expect("wide frame"));
        let compact_frame = read_server_frame(compact_rx.recv().expect("compact frame"));
        let wide_text = frame_text(&wide_frame);
        let compact_text = frame_text(&compact_frame);
        assert!(
            wide_text.contains("breathe in"),
            "wide prompt: {wide_text:?}"
        );
        assert!(wide_text.contains('█'), "wide orb: {wide_text:?}");
        assert!(
            !compact_text.contains("take a deep breath."),
            "compact prompt has no orb layout: {compact_text:?}"
        );
        acknowledge_latest_test_render(&mut server, 1);
        acknowledge_latest_test_render(&mut server, 2);

        let shown_at = Instant::now();
        server.app.state.pomodoro.prompt = None;
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at,
        });
        assert_eq!(server.app.state.screen_rect(), Rect::new(0, 0, 18, 30));
        assert_eq!(
            server.pomodoro_animation_deadline(
                shown_at + crate::pomodoro::SEND_OFF_DURATION - Duration::from_millis(1)
            ),
            Some(shown_at + crate::pomodoro::SEND_OFF_DURATION)
        );
        server.render_and_stream();

        let wide_send_off = read_server_frame(wide_rx.recv().expect("wide send-off frame"));
        let compact_send_off = read_server_frame(compact_rx.recv().expect("compact clear frame"));
        assert!(frame_text(&wide_send_off).contains("enjoy the break"));
        assert!(!frame_text(&compact_send_off).contains("enjoy the break"));
        acknowledge_latest_test_render(&mut server, 1);
        acknowledge_latest_test_render(&mut server, 2);

        let expiry = shown_at + crate::pomodoro::SEND_OFF_DURATION;
        assert!(server.handle_scheduled_tasks_headless(expiry, false));
        assert!(server.app.state.pomodoro.send_off.is_none());
        server.render_and_stream();
        assert!(!frame_text(&read_server_frame(
            wide_rx.recv().expect("wide expiry frame")
        ))
        .contains("enjoy the break"));
        acknowledge_latest_test_render(&mut server, 1);

        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            KeyModifiers::empty(),
        );
        let before_expiry = Instant::now();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: before_expiry,
        });
        server.render_and_stream();
        assert!(frame_text(&read_server_frame(
            wide_rx.recv().expect("wide second send-off frame")
        ))
        .contains("enjoy the break"));
        acknowledge_latest_test_render(&mut server, 1);
        assert!(server.handle_client_input_events(
            2,
            vec![
                crate::raw_input::RawInputEvent::Key(key.clone()),
                crate::raw_input::RawInputEvent::Key(
                    key.clone()
                        .with_kind(crossterm::event::KeyEventKind::Release),
                ),
            ],
        ));
        assert!(server.app.state.pomodoro.send_off.is_some());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), pane_input.recv())
                .await
                .expect("compact-client key timed out")
                .expect("compact-client key reaches pane"),
            Bytes::from_static(b"x")
        );

        assert!(server.handle_client_input_events(
            1,
            vec![
                crate::raw_input::RawInputEvent::Key(key.clone()),
                crate::raw_input::RawInputEvent::Key(
                    key.clone()
                        .with_kind(crossterm::event::KeyEventKind::Release),
                ),
            ],
        ));
        assert!(server.app.state.pomodoro.send_off.is_none());
        assert!(
            pane_input.try_recv().is_err(),
            "wide-client dismissal key reached the pane"
        );
        server.render_and_stream();
        assert!(!frame_text(&read_server_frame(
            wide_rx.recv().expect("wide dismissal frame")
        ))
        .contains("enjoy the break"));
        acknowledge_latest_test_render(&mut server, 1);

        let expired_at = Instant::now();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: expired_at - crate::pomodoro::SEND_OFF_DURATION,
        });
        assert!(server.handle_scheduled_tasks_headless(expired_at, false));
        assert!(server.app.state.pomodoro.send_off.is_none());
        for client_id in [1, 2] {
            assert!(server.handle_client_input_events(
                client_id,
                vec![crate::raw_input::RawInputEvent::Key(key.clone())],
            ));
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), pane_input.recv())
                    .await
                    .expect("post-expiry key timed out")
                    .expect("post-expiry key reaches pane"),
                Bytes::from_static(b"x"),
                "post-expiry key from client {client_id}"
            );
        }
    }

    #[tokio::test]
    async fn p1_headless_input_batch_uses_its_starting_pomodoro_gate() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let (client_tx, _control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        server.render_and_stream();
        let _ = read_server_frame(client_rx.recv().expect("committed send-off frame"));

        let key = |ch| {
            crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char(ch),
                KeyModifiers::empty(),
            ))
        };
        assert!(server.handle_client_input_events(1, vec![key('x'), key('y')]));
        assert!(server.app.state.pomodoro.send_off.is_none());
        assert!(
            pane_input.try_recv().is_err(),
            "an event after dismissal escaped the batch's starting gate"
        );
    }

    #[tokio::test]
    async fn p1_headless_focus_return_batch_refreshes_the_pomodoro_gate() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let now = Instant::now();
        server.app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            now,
        );
        let (client_tx, _control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.handle_scheduled_tasks_headless(now + Duration::from_secs(60), false);
        assert!(server.app.state.pomodoro.held());
        server.resize_shared_runtime_to_effective_size();
        server.render_and_stream();
        let _ = read_server_frame(client_rx.recv().expect("committed pre-focus frame"));

        let key = crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            KeyModifiers::empty(),
        ));
        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::OuterFocusGained, key],
        ));
        assert!(server.app.state.pomodoro.prompt.is_some());
        assert!(
            pane_input.try_recv().is_err(),
            "a key after focus return must not reach the pane"
        );
    }

    #[tokio::test]
    async fn p1_headless_focus_return_gate_survives_until_a_frame_is_committed() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let now = Instant::now();
        server.app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            now,
        );
        let (client_tx, _control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.handle_scheduled_tasks_headless(now + Duration::from_secs(60), false);
        assert!(server.app.state.pomodoro.held());
        server.resize_shared_runtime_to_effective_size();
        server.render_and_stream();
        let _ = read_server_frame(client_rx.recv().expect("committed pre-focus frame"));

        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::OuterFocusGained],
        ));
        assert!(server.app.state.pomodoro.prompt.is_some());
        let key = crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            KeyModifiers::empty(),
        ));
        assert!(server.handle_client_input_events(1, vec![key]));
        assert!(
            pane_input.try_recv().is_err(),
            "the prompt raised between frames must gate the next batch"
        );
    }

    #[tokio::test]
    async fn p1_headless_prompt_gates_input_before_the_clients_first_frame() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let now = Instant::now();
        server.app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                work_minutes: 1,
                ..Default::default()
            },
            now,
        );
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(test_client_writer().0),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.handle_scheduled_tasks_headless(now + Duration::from_secs(60), false);
        assert!(server.app.state.pomodoro.held());
        server.resize_shared_runtime_to_effective_size();

        let key = crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            KeyModifiers::empty(),
        ));
        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::OuterFocusGained, key],
        ));
        assert!(server.app.state.pomodoro.prompt.is_some());
        assert!(
            pane_input.try_recv().is_err(),
            "the pre-frame prompt must gate input after a focus return"
        );
    }

    #[tokio::test]
    async fn p1_headless_focus_refresh_cannot_remove_another_clients_visible_gate() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let (client_one_tx, _client_one_control_rx, client_one_rx) = test_client_writer();
        let (client_two_tx, _client_two_control_rx, client_two_rx) = test_client_writer();
        for (client_id, writer) in [(1, client_one_tx), (2, client_two_tx)] {
            server.clients.insert(
                client_id,
                ClientConnection::new(
                    (100, 30),
                    crate::kitty_graphics::HostCellSize::default(),
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    client_id,
                    RenderEncoding::SemanticFrame,
                    Some(writer),
                ),
            );
        }
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        server.render_and_stream();
        let _ = read_server_frame(client_one_rx.recv().expect("client one send-off frame"));
        let _ = read_server_frame(client_two_rx.recv().expect("client two send-off frame"));

        let key = |ch| {
            crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char(ch),
                KeyModifiers::empty(),
            ))
        };
        assert!(server.handle_client_input_events(2, vec![key('x')]));
        assert!(server.app.state.pomodoro.send_off.is_none());
        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::OuterFocusGained, key('y')],
        ));
        assert!(
            pane_input.try_recv().is_err(),
            "client two cannot clear the gate still visible to client one"
        );
    }

    #[tokio::test]
    async fn p1_headless_send_off_gate_stays_owned_until_clear_frame_flushes() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let (client_tx, _control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });

        server.render_and_stream();
        let _ = read_server_frame(client_rx.recv().expect("send-off frame"));
        let send_off_sequence = server.clients[&1]
            .latest_pending_pomodoro_render_sequence()
            .expect("pending send-off presentation");
        assert!(
            !server.handle_server_event(ServerEvent::ClientWriterDrained {
                client_id: 1,
                render_sequence: send_off_sequence,
            })
        );

        let key = |ch| {
            crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char(ch),
                KeyModifiers::empty(),
            ))
        };
        assert!(server.handle_client_input_events(1, vec![key('x')]));
        assert!(server.app.state.pomodoro.send_off.is_none());

        server.render_and_stream();
        let clear_sequence = server.clients[&1]
            .latest_pending_pomodoro_render_sequence()
            .expect("pending clear presentation");
        assert!(server.clients[&1].pomodoro_presentation.send_off.is_some());
        assert!(server.handle_client_input_events(1, vec![key('y')]));
        assert!(pane_input.try_recv().is_err());

        let clear_frame = read_server_frame(client_rx.recv().expect("clear frame"));
        assert!(!frame_text(&clear_frame).contains("enjoy the break"));
        assert!(
            !server.handle_server_event(ServerEvent::ClientWriterDrained {
                client_id: 1,
                render_sequence: clear_sequence,
            })
        );
        assert!(server.clients[&1].pomodoro_presentation.send_off.is_none());

        assert!(server.handle_client_input_events(1, vec![key('z')]));
        assert_eq!(
            pane_input
                .recv()
                .await
                .expect("post-flush key reaches pane"),
            Bytes::from_static(b"z")
        );
    }

    #[tokio::test]
    async fn p1_headless_send_off_repeat_preserves_the_forwarded_release_lease() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"\x1b[>15u");
        let (client_tx, _control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.render_and_stream();
        let _ = read_server_frame(client_rx.recv().expect("initial frame"));

        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            KeyModifiers::empty(),
        );
        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::Key(key.clone())],
        ));
        assert_eq!(
            pane_input.recv().await.expect("forwarded press"),
            Bytes::from_static(b"\x1b[120;1:1u")
        );

        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        server.render_and_stream();
        let _ = read_server_frame(client_rx.recv().expect("send-off frame"));
        assert!(server.handle_client_input_events(
            1,
            vec![
                crate::raw_input::RawInputEvent::Key(
                    key.clone()
                        .with_kind(crossterm::event::KeyEventKind::Repeat),
                ),
                crate::raw_input::RawInputEvent::Key(
                    key.with_kind(crossterm::event::KeyEventKind::Release),
                ),
            ],
        ));
        assert_eq!(
            pane_input.try_recv().expect("forwarded repeat"),
            Bytes::from_static(b"\x1b[120;1:2u")
        );
        assert_eq!(
            pane_input.try_recv().expect("forwarded release"),
            Bytes::from_static(b"\x1b[120;1:3u")
        );
        assert!(server.app.input_leases.is_empty());
        assert!(server.app.state.pomodoro.send_off.is_some());
    }

    #[tokio::test]
    async fn p2_headless_moved_send_off_dismissal_requests_repaint() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let (client_tx, _control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        server.render_and_stream();
        let _ = read_server_frame(client_rx.recv().expect("committed send-off frame"));
        assert!(
            server.clients[&1].pomodoro_presentation.send_off.is_some(),
            "committed presentation: {:?}",
            server.clients[&1].pomodoro_presentation
        );
        assert!(server.clients[&1].render_state.last_frame().is_some());

        let moved = crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        });
        assert!(server.handle_client_input_events(1, vec![moved]));
        assert!(server.app.state.pomodoro.send_off.is_none());
        assert!(server.clients[&1].render_state.last_frame().is_none());
        assert!(pane_input.try_recv().is_err());

        server.render_and_stream();
        let frame = read_server_frame(client_rx.recv().expect("repaint after dismissal"));
        assert!(!frame_text(&frame).contains("enjoy the break"));
    }

    #[tokio::test]
    async fn p4_headless_resize_before_render_keeps_the_committed_pomodoro_gate() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let (wide_tx, _wide_control_rx, wide_rx) = test_client_writer();
        let (compact_tx, _compact_control_rx, compact_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(wide_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (18, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(compact_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        server.render_and_stream();
        assert!(
            frame_text(&read_server_frame(wide_rx.recv().expect("wide frame")))
                .contains("enjoy the break")
        );
        assert!(!frame_text(&read_server_frame(
            compact_rx.recv().expect("compact frame")
        ))
        .contains("enjoy the break"));
        acknowledge_latest_test_render(&mut server, 1);
        acknowledge_latest_test_render(&mut server, 2);

        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 1,
            cols: 18,
            rows: 30,
            cell_width_px: 0,
            cell_height_px: 0,
        }));
        let key = |ch| {
            crate::raw_input::RawInputEvent::Key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char(ch),
                KeyModifiers::empty(),
            ))
        };
        assert!(server.handle_client_input_events(1, vec![key('x')]));
        assert!(server.app.state.pomodoro.send_off.is_none());
        assert!(pane_input.try_recv().is_err());

        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 2,
            cols: 100,
            rows: 30,
            cell_width_px: 0,
            cell_height_px: 0,
        }));
        assert!(server.handle_client_input_events(2, vec![key('y')]));
        assert!(server.app.state.pomodoro.send_off.is_some());
        assert_eq!(
            pane_input
                .recv()
                .await
                .expect("unseen-card key reaches pane"),
            Bytes::from_static(b"y")
        );
    }

    #[tokio::test]
    async fn f2_headless_send_off_release_clears_the_dismissal_key_lease() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"");
        let (client_tx, _control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        server
            .clients
            .get_mut(&1)
            .expect("client")
            .pomodoro_presentation = crate::ui::pomodoro::input_presentation_at(
            &server.app.state,
            Rect::new(0, 0, 100, 30),
            Instant::now(),
        );
        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            KeyModifiers::empty(),
        );

        assert!(server.handle_client_input_events(
            1,
            vec![
                crate::raw_input::RawInputEvent::Key(key.clone()),
                crate::raw_input::RawInputEvent::Key(
                    key.clone()
                        .with_kind(crossterm::event::KeyEventKind::Repeat),
                ),
            ],
        ));
        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        assert!(server.handle_client_input_events(
            1,
            vec![crate::raw_input::RawInputEvent::Key(
                key.clone()
                    .with_kind(crossterm::event::KeyEventKind::Release),
            )],
        ));
        assert!(server.app.input_leases.is_empty());
        assert!(server.app.state.pomodoro.send_off.is_some());
        assert!(pane_input.try_recv().is_err());

        server.app.state.pomodoro.send_off = None;
        server
            .clients
            .get_mut(&1)
            .expect("client")
            .pomodoro_presentation = crate::ui::pomodoro::InputPresentation {
            area: Rect::new(0, 0, 100, 30),
            ..Default::default()
        };
        assert!(
            server.handle_client_input_events(1, vec![crate::raw_input::RawInputEvent::Key(key)],)
        );
        assert_eq!(
            pane_input.recv().await.expect("second press reaches pane"),
            Bytes::from_static(b"x")
        );
    }

    #[tokio::test]
    async fn f3_headless_send_off_only_swallows_mouse_up_after_its_down() {
        let mut server = test_headless_server();
        let mut pane_input = install_focused_test_runtime(&mut server, b"\x1b[?1003h\x1b[?1006h");
        let (client_tx, _control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        let mouse = |kind| {
            crate::raw_input::RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 40,
                row: 12,
                modifiers: KeyModifiers::empty(),
            })
        };

        assert!(server.handle_client_input_events(
            1,
            vec![mouse(MouseEventKind::Down(
                crossterm::event::MouseButton::Left,
            ))],
        ));
        pane_input.recv().await.expect("mouse down reaches pane");

        server.app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: crate::pomodoro::PomodoroPhase::ShortBreak,
            shown_at: Instant::now(),
        });
        server
            .clients
            .get_mut(&1)
            .expect("client")
            .pomodoro_presentation = crate::ui::pomodoro::input_presentation_at(
            &server.app.state,
            Rect::new(0, 0, 100, 30),
            Instant::now(),
        );
        assert!(server.handle_client_input_events(
            1,
            vec![mouse(MouseEventKind::Up(
                crossterm::event::MouseButton::Left,
            ))],
        ));
        pane_input.recv().await.expect("mouse up reaches pane");
        assert!(server.app.state.pomodoro.send_off.is_some());

        assert!(server.handle_client_input_events(
            1,
            vec![mouse(MouseEventKind::Down(
                crossterm::event::MouseButton::Left,
            ))],
        ));
        assert!(server.handle_client_input_events(
            1,
            vec![mouse(MouseEventKind::Up(
                crossterm::event::MouseButton::Left,
            ))],
        ));
        assert!(pane_input.try_recv().is_err());
    }

    #[tokio::test]
    async fn resize_shared_runtime_resizes_background_tabs() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let background_tab = workspace.test_add_tab(Some("background"));
        let active_pane = workspace.tabs[0].root_pane;
        let background_pane = workspace.tabs[background_tab].root_pane;
        workspace.tabs[0].runtimes.insert(
            active_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        workspace.tabs[background_tab].runtimes.insert(
            background_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        let terminal_area = server.app.state.view.terminal_area;
        let expected = (terminal_area.height, terminal_area.width.saturating_sub(1));
        assert_eq!(
            server
                .app
                .state
                .runtime_for_pane(&server.app.terminal_runtimes, active_pane)
                .unwrap()
                .current_size(),
            expected
        );
        assert_eq!(
            server
                .app
                .state
                .runtime_for_pane(&server.app.terminal_runtimes, background_pane)
                .unwrap()
                .current_size(),
            expected
        );
    }

    #[test]
    fn terminal_attach_uses_smallest_client_geometry_and_preserves_known_pixels() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).expect("terminal id").clone();
        let terminal_id_string = terminal_id.to_string();
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.terminal_runtimes.insert(
            terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        server.clients.insert(
            1,
            ClientConnection::new(
                (199, 50),
                crate::kitty_graphics::HostCellSize {
                    width_px: 9,
                    height_px: 18,
                },
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        let expected_app_size = server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .current_size();
        assert_eq!(expected_app_size.1, 171);

        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 305,
            rows: 80,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            direct_graphics: false,
            writer,
        }));
        assert!(
            server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 2,
                terminal_id: terminal_id_string.clone(),
                takeover: false,
            })
        );
        assert_eq!(server.foreground_client_id, Some(1));
        assert!(server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size(),
            expected_app_size
        );
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .pixel_size(),
            Some((
                u32::from(expected_app_size.1) * 9,
                u32::from(expected_app_size.0) * 18
            ))
        );

        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 1,
            cols: 388,
            rows: 60,
            cell_width_px: 9,
            cell_height_px: 18,
        }));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size(),
            (59, 305)
        );
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .pixel_size(),
            Some((305 * 9, 59 * 18))
        );

        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 2,
            cols: 305,
            rows: 100,
            cell_width_px: 12,
            cell_height_px: 16,
        }));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size(),
            (59, 305)
        );
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .pixel_size(),
            Some((305 * 9, 59 * 16))
        );
        let widened_app_size = server.clients[&1].terminal_geometries[&terminal_id];

        assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 2 }));

        assert!(!server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size(),
            (widened_app_size.rows, widened_app_size.cols)
        );
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .pixel_size(),
            Some((
                u32::from(widened_app_size.cols) * 9,
                u32::from(widened_app_size.rows) * 18,
            ))
        );

        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 305,
            rows: 100,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            direct_graphics: false,
            writer,
        }));
        assert!(
            server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 2,
                terminal_id: terminal_id_string,
                takeover: false,
            })
        );
        assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 1 }));
        let unknown_only_size = server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .current_size();
        assert_eq!(unknown_only_size, (100, 305));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .pixel_size(),
            Some((305 * 9, 100 * 18)),
            "an unknown-only client must not erase the last known cell size"
        );
        drop(server);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn render_and_stream_sends_terminal_frame_for_terminal_ansi_client() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();

        match read_server_message(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("terminal frame"),
        ) {
            ServerMessage::Terminal(frame) => {
                assert_eq!(frame.seq, 1);
                assert_eq!((frame.width, frame.height), (80, 24));
                assert!(frame.full);
                assert!(!frame.bytes.is_empty());
            }
            other => panic!("expected terminal frame, got {other:?}"),
        }
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
    }

    #[test]
    fn render_and_stream_sends_large_terminal_frame_for_terminal_ansi_client() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (278, 85),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        server.render_and_stream();
        match read_server_message(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial terminal frame"),
        ) {
            ServerMessage::Terminal(frame) => {
                assert_eq!(frame.seq, 1);
                assert_eq!((frame.width, frame.height), (278, 85));
                assert!(frame.full);
            }
            other => panic!("expected terminal frame, got {other:?}"),
        }

        assert!(server.handle_server_event(ServerEvent::ClientResize {
            client_id: 1,
            cols: 710,
            rows: 202,
            cell_width_px: 0,
            cell_height_px: 0,
        }));
        server.render_and_stream();

        match read_server_message(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("large terminal frame"),
        ) {
            ServerMessage::Terminal(frame) => {
                assert_eq!(frame.seq, 2);
                assert_eq!((frame.width, frame.height), (710, 202));
                assert!(frame.full);
                assert!(!frame.bytes.is_empty());
            }
            other => panic!("expected terminal frame, got {other:?}"),
        }

        server.app.state.set_server_mode(crate::app::Mode::Navigate);
        server.render_and_stream();
        match read_server_message(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("follow-up terminal frame"),
        ) {
            ServerMessage::Terminal(frame) => assert_eq!(frame.seq, 3),
            other => panic!("expected terminal frame, got {other:?}"),
        }
    }

    #[test]
    fn terminal_ansi_input_does_not_reset_blit_baseline() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial terminal frame");
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );

        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: Vec::new(),
        }));
        server.render_and_stream();

        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn outer_focus_gained_repaints_terminal_ansi_without_clearing() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial terminal frame");

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));
        server.render_and_stream();

        match read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()) {
            ServerMessage::Terminal(frame) => {
                assert_eq!(frame.seq, 2);
                assert!(frame.full);
                assert!(!frame.bytes.windows(4).any(|bytes| bytes == b"\x1b[2J"));
            }
            other => panic!("expected terminal frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn outer_focus_gained_client_render_pending_survives_semantic_render_queue_full() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial semantic frame");

        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        server.clients[&1]
            .writer
            .as_ref()
            .unwrap()
            .test_fill_render(queued);

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));
        assert_eq!(
            server.clients.get(&1).unwrap().deferred_render(),
            DeferredRender::Full
        );

        server.render_and_stream();

        assert_eq!(
            server.clients.get(&1).unwrap().deferred_render(),
            DeferredRender::Full
        );
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());

        assert!(
            server.handle_server_event(ServerEvent::ClientWriterDrained {
                client_id: 1,
                render_sequence: 1,
            })
        );
        server.render_and_stream();

        assert_eq!(
            server.clients.get(&1).unwrap().deferred_render(),
            DeferredRender::None
        );
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::Frame(_)
        ));
    }

    #[test]
    fn outer_focus_gained_does_not_force_terminal_ansi_full_redraw_when_disabled() {
        let mut server = test_headless_server();
        server.app.state.redraw_on_focus_gained = false;
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial terminal frame");

        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        });
        server.render_and_stream();

        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(true));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
    }

    #[test]
    fn outer_focus_gained_does_not_mark_semantic_render_pending_when_disabled() {
        let mut server = test_headless_server();
        server.app.state.redraw_on_focus_gained = false;
        let (client_tx, _client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));

        assert_eq!(
            server.clients.get(&1).unwrap().deferred_render(),
            DeferredRender::None
        );
        assert!(!server.app.full_redraw_pending);
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(true));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
    }

    #[test]
    fn full_render_queue_does_not_advance_terminal_ansi_baseline() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();
        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        client_tx.test_fill_render(queued);

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();

        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            0
        );
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn writer_drained_retries_pending_terminal_ansi_render() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();
        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        client_tx.test_fill_render(queued);

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        assert_eq!(
            server.clients.get(&1).unwrap().deferred_render(),
            DeferredRender::Full
        );
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));

        assert!(
            server.handle_server_event(ServerEvent::ClientWriterDrained {
                client_id: 1,
                render_sequence: 1,
            })
        );
        server.render_and_stream();

        match read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()) {
            ServerMessage::Terminal(frame) => assert_eq!(frame.seq, 1),
            other => panic!("expected terminal frame, got {other:?}"),
        }
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
        assert_eq!(
            server.clients.get(&1).unwrap().deferred_render(),
            DeferredRender::None
        );
    }

    #[test]
    fn render_and_stream_skips_identical_frame_sends() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        server.render_and_stream();
        let first = client_rx.recv_timeout(Duration::from_millis(100));
        assert!(first.is_ok(), "expected first frame to be sent");

        server.render_and_stream();
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "identical frame should not be sent twice"
        );
    }

    #[test]
    fn visible_source_wakes_pending_hidden_work() {
        let (server, background_pane) = hidden_pty_visibility_test_server(&[(120, 40)]);
        let visible_pane = server.app.state.workspaces[0].tabs[0].root_pane;
        server.sync_immediate_pty_sources();

        assert!(server.app.render_dirty.request_pty(background_pane));
        assert!(!server.has_pending_presentation_work(false, false));
        assert!(server.app.render_dirty.request_pty(visible_pane));
        assert!(server.has_pending_presentation_work(false, false));
    }

    #[test]
    fn inactive_tab_pty_source_is_hidden_until_tab_focus() {
        let (server, background_pane) = hidden_pty_visibility_test_server(&[]);
        let sources = HashSet::from([background_pane]);
        assert!(!server.pty_sources_visible_to_any_render_target(&sources));

        let (mut server, background_pane) =
            hidden_pty_visibility_test_server(&[(120, 40), (44, 20)]);
        let sources = HashSet::from([background_pane]);
        assert!(!server.pty_sources_visible_to_any_render_target(&sources));

        server.app.state.workspaces[0].switch_tab(1);
        assert!(server.pty_sources_visible_to_any_render_target(&sources));
    }

    #[tokio::test]
    async fn hidden_pty_output_appears_after_switching_to_its_tab() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let background_tab = workspace.test_add_tab(Some("background"));
        let background_pane = workspace.tabs[background_tab].root_pane;
        workspace.insert_test_runtime(
            background_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"before"),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let (client_tx, _client_control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        server.render_and_stream();
        let _initial_frame = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, background_pane)
            .expect("background runtime");
        runtime.test_process_pty_bytes(b"\rhidden-update");
        assert!(server.app.render_dirty.request_pty(background_pane));
        let request = server.app.render_dirty.take();
        let pty = if server.pty_sources_visible_to_any_render_target(&request.pty_sources) {
            PtyRenderState::Visible
        } else {
            PtyRenderState::Hidden
        };
        assert_eq!(
            retained_render_plan(RetainedRenderInput {
                needs_full_render: false,
                needs_graphics_render: false,
                pty,
            }),
            RetainedRenderPlan::HiddenPty
        );
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());

        server.app.state.workspaces[0].switch_tab(background_tab);
        server.render_and_stream();
        let visible_frame = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("frame after tab switch"),
        );
        assert!(frame_text(&visible_frame).contains("hidden-update"));
    }

    #[test]
    fn direct_terminal_observer_keeps_hidden_pty_source_renderable_with_app_client() {
        let (mut server, background_pane) = hidden_pty_visibility_test_server(&[(120, 40)]);
        assert!(!server.pty_sources_visible_to_any_render_target(&HashSet::from([background_pane])));

        let terminal_id = server.app.state.workspaces[0]
            .terminal_id(background_pane)
            .expect("background terminal id")
            .to_string();
        let (client_tx, _client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            2,
            ClientConnection::new_with_mode(
                ClientConnectionMode::TerminalObserve { terminal_id },
                None,
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                false,
                Some(client_tx),
            ),
        );

        assert!(server.pty_sources_visible_to_any_render_target(&HashSet::from([background_pane])));

        let hidden_pane = server.app.state.workspaces[0].tabs[0].root_pane;
        server.sync_immediate_pty_sources();
        assert!(server.app.render_dirty.request_pty(background_pane));
        assert!(server.has_pending_presentation_work(false, false));
        assert!(server.app.render_dirty.request_pty(hidden_pane));
    }

    #[tokio::test]
    async fn retained_pty_update_streams_dirty_row_from_last_frame() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        assert!(!server.app.state.tab_surface_replaced());
        assert!(server.app.state.app_surface_pane_ids().contains(&pane_id));
        assert!(server.app_surface_contains_pane(pane_id));
        assert!(server.retained_pty_update_allowed_by_app_state());
        server.render_and_stream();
        let first = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial frame"),
        );
        assert!(first.cells.iter().any(|cell| cell.symbol == "a"));

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(server.render_retained_pty_update_and_stream());
        let patched = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame"),
        );
        assert!(patched.cells.iter().any(|cell| cell.symbol == "Z"));
        assert_eq!((patched.width, patched.height), (80, 24));
    }

    #[tokio::test]
    async fn tab_client_keeps_retained_pty_updates_beside_attach_local_usage_client() {
        let (mut server, usage_rx, pane_id) = retained_test_server(b"aaaa");
        let (tab_tx, _tab_control_rx, tab_rx) = test_client_writer();
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(tab_tx),
            ),
        );
        server.render_and_stream();
        let _ = usage_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial first-client frame");
        let tab_frame = read_server_frame(
            tab_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial tab-client frame"),
        );

        open_client_usage(&mut server, 1, &usage_rx);
        server.render_and_stream();
        let usage_frame = read_server_frame(
            usage_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("usage frame"),
        );
        assert!(tab_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(server.clients[&1].usage_view.is_some());
        assert!(server.clients[&2].usage_view.is_none());
        assert!(server.any_app_client_displays_tab());
        server.sync_immediate_pty_sources();
        assert!(server.app.render_dirty.request_pty(pane_id));

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(server.render_retained_pty_update_and_stream());
        assert_eq!(
            server.clients[&1]
                .render_state
                .last_frame()
                .expect("usage frame retained"),
            &usage_frame
        );
        assert!(usage_rx.recv_timeout(Duration::from_millis(50)).is_err());
        let patched_tab = read_server_frame(
            tab_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained tab frame"),
        );
        assert_ne!(patched_tab, tab_frame);
        assert!(patched_tab.cells.iter().any(|cell| cell.symbol == "Z"));
    }

    #[tokio::test]
    async fn retained_pty_update_cannot_alternate_home_composer_cells() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"underlying pane");
        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial pane frame");
        server.app.state.home = Some(crate::app::home::HomeState::default());
        server.render_and_stream();
        let home_frame = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial home frame"),
        );
        assert!(!server.app.state.app_surface_pane_ids().contains(&pane_id));
        assert!(!server.app_surface_contains_pane(pane_id));
        let pane = server.app.state.view.pane_infos[0].clone();
        let top_right = (pane.inner_rect.right() - 2, pane.inner_rect.y);
        let bottom_left = (pane.inner_rect.x, pane.inner_rect.bottom() - 2);
        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        let (rows, cols) = runtime.current_size();
        runtime.test_process_pty_bytes(
            format!(
                "\x1b[1;{}HX\x1b[{};1HY",
                cols.saturating_sub(1),
                rows.saturating_sub(1)
            )
            .as_bytes(),
        );

        let retained = server.render_retained_pty_update_and_stream();
        let retained_frame = server
            .clients
            .get(&1)
            .and_then(|client| client.render_state.last_frame())
            .expect("client frame after retained attempt");
        let cell_index = |frame: &FrameData, (x, y): (u16, u16)| {
            usize::from(y) * usize::from(frame.width) + usize::from(x)
        };

        assert_eq!(
            retained_frame.cells[cell_index(retained_frame, top_right)],
            home_frame.cells[cell_index(&home_frame, top_right)]
        );
        assert_eq!(
            retained_frame.cells[cell_index(retained_frame, bottom_left)],
            home_frame.cells[cell_index(&home_frame, bottom_left)]
        );
        assert_eq!(retained_frame, &home_frame);
        assert!(!retained, "home must reject tiled-pane retained patches");
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "unchanged home cells must not be streamed"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_declines_while_popup_is_visible() {
        let (mut server, client_rx, _) = retained_test_server(b"tiled");
        let popup_runtime =
            crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 12, b"popup-aaaa");
        let (_, terminal_id) = server.app.install_test_popup_runtime(popup_runtime);

        server.render_and_stream();
        let initial = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial popup frame"),
        );
        assert!(frame_text(&initial).contains("popup-aaaa"));
        server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        server.render_and_stream();
        let updated = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("full popup fallback frame"),
        );
        assert!(frame_text(&updated).contains("Zopup-aaaa"));
    }

    #[tokio::test]
    async fn retained_pty_update_declines_for_dock_owned_confirmation() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");
        acknowledge_latest_test_render(&mut server, 1);
        server
            .clients
            .get_mut(&1)
            .expect("client")
            .dock_presentation
            .pr_action_confirmation = Some(crate::app::state::PrActionConfirmation {
            key: crate::app::state::WorkItemKey {
                repo: "owner/repo".into(),
                pr_number: Some(42),
                pr_url: Some("https://github.com/owner/repo/pull/42".into()),
                ticket_id: None,
            },
            action: crate::ui::work_list_detail::PrActionKind::Merge(
                crate::config::MergeMethodConfig::Rebase,
            ),
        });
        server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime")
            .test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[tokio::test]
    async fn popup_forces_host_mouse_capture_for_headless_client() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.app.state.mouse_capture = false;
        let popup_runtime =
            crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 12, b"popup");
        server.app.install_test_popup_runtime(popup_runtime);

        server.stream_host_mouse_capture_mode();

        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("mouse capture message")
            ),
            ServerMessage::MouseCapture { enabled: true, .. }
        ));
    }

    #[tokio::test]
    async fn command_mode_updates_headless_client_keyboard_flags() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );

        server.app.state.set_server_mode(crate::app::Mode::Prefix);
        server.stream_host_keyboard_enhancement_flags();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("command-mode keyboard enhancement message")
            ),
            ServerMessage::KittyKeyboardReportAll { enabled: true }
        ));

        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.stream_host_keyboard_enhancement_flags();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("IME-compatible keyboard enhancement message")
            ),
            ServerMessage::KittyKeyboardReportAll { enabled: false }
        ));
    }

    #[tokio::test]
    async fn focused_report_all_pane_updates_headless_client_keyboard_flags() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        let popup_runtime =
            crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 12, b"\x1b[>15u");
        server.app.install_test_popup_runtime(popup_runtime);

        server.stream_host_keyboard_enhancement_flags();

        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("keyboard enhancement message")
            ),
            ServerMessage::KittyKeyboardReportAll { enabled: true }
        ));

        assert!(server.app.close_popup_pane());
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.stream_host_keyboard_enhancement_flags();
        assert!(matches!(
            read_server_message(
                client_control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("IME-compatible keyboard enhancement message")
            ),
            ServerMessage::KittyKeyboardReportAll { enabled: false }
        ));
    }

    #[tokio::test]
    async fn virtual_render_uses_popup_cursor() {
        let (mut server, _client_rx, _) = retained_test_server(b"\x1b[2;2H");
        let popup_runtime =
            crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 12, b"\x1b[4;5H");
        let (_, terminal_id) = server.app.install_test_popup_runtime(popup_runtime);

        let (_, cursor) = crate::server::render_stream::render_virtual_with_runtime_registry(
            &mut server.app.state,
            &server.app.terminal_runtimes,
            ratatui::layout::Rect::new(0, 0, 80, 24),
            true,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let (_, inner) =
            crate::ui::popup_pane_rects(&server.app.state, server.app.state.view.terminal_area)
                .unwrap();
        let expected = server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .cursor_state(inner, true)
            .unwrap();

        assert_eq!(
            cursor,
            Some(crate::protocol::CursorState {
                x: expected.x,
                y: expected.y,
                visible: expected.visible,
                shape: expected.shape,
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_does_not_resize_directly_attached_popup() {
        let (mut server, _client_rx, _) = retained_test_server(b"tiled");
        let popup_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(50, 13, b"");
        let (_, terminal_id) = server.app.install_test_popup_runtime(popup_runtime);
        server
            .app
            .state
            .direct_attach_resize_locks
            .insert(terminal_id.clone());

        let _ = crate::server::render_stream::render_virtual_with_runtime_registry(
            &mut server.app.state,
            &server.app.terminal_runtimes,
            ratatui::layout::Rect::new(0, 0, 80, 24),
            true,
            crate::kitty_graphics::HostCellSize::default(),
        );

        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .current_size(),
            (13, 50)
        );
    }

    #[tokio::test]
    async fn direct_attach_keeps_popup_at_smallest_client_geometry() {
        let (mut server, _client_rx, _) = retained_test_server(b"tiled");
        let popup_runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(50, 13, b"");
        let (_, terminal_id) = server.app.install_test_popup_runtime(popup_runtime);

        server.render_and_stream();
        let (_, popup_inner) =
            crate::ui::popup_pane_rects(&server.app.state, server.app.state.view.terminal_area)
                .expect("popup geometry");
        let expected = (popup_inner.height, popup_inner.width);
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("popup runtime")
                .current_size(),
            expected
        );

        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 305,
            rows: 80,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            direct_graphics: false,
            writer,
        }));
        assert!(
            server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 2,
                terminal_id: terminal_id.to_string(),
                takeover: false,
            })
        );

        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("popup runtime")
                .current_size(),
            expected,
            "the wider direct attach must not widen a popup past the GUI layout"
        );
    }

    #[test]
    fn unknown_app_preserves_runtime_pixels_after_known_gui_disconnects() {
        with_terminal_session_test_server(|server, terminal_id, _, _| {
            server.app.state.active = Some(0);
            server.app.state.selected = 0;
            server.app.state.set_server_mode(crate::app::Mode::Terminal);
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .resize(24, 80, 9, 18);

            server.clients.insert(
                1,
                ClientConnection::new(
                    (200, 50),
                    crate::kitty_graphics::HostCellSize {
                        width_px: 9,
                        height_px: 18,
                    },
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    2,
                    RenderEncoding::SemanticFrame,
                    None,
                ),
            );
            server.clients.insert(
                2,
                ClientConnection::new(
                    (100, 30),
                    crate::kitty_graphics::HostCellSize::default(),
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    1,
                    RenderEncoding::SemanticFrame,
                    None,
                ),
            );

            server.foreground_client_id = Some(1);
            server.sync_foreground_client_state();
            server.resize_shared_runtime_to_effective_size();
            server.foreground_client_id = Some(2);
            server.sync_foreground_client_state();
            server.resize_shared_runtime_to_effective_size();
            let unknown_geometry = server.clients[&2].terminal_geometries[&terminal_id];
            server.foreground_client_id = Some(1);
            server.sync_foreground_client_state();
            server.resize_shared_runtime_to_effective_size();

            assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 1 }));
            let runtime = server.app.terminal_runtimes.get(&terminal_id).unwrap();
            assert_eq!(
                runtime.current_size(),
                (unknown_geometry.rows, unknown_geometry.cols)
            );
            assert_eq!(
                runtime.pixel_size(),
                Some((
                    u32::from(unknown_geometry.cols) * 9,
                    u32::from(unknown_geometry.rows) * 18,
                )),
                "an unknown app client must preserve the runtime's known cell size"
            );
        });
    }

    #[tokio::test]
    async fn retained_pty_update_declines_while_toast_is_visible() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".to_owned(),
            context: "background · 2".to_owned(),
            position: None,
            target: None,
        });
        server.render_and_stream();
        let initial = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial frame"),
        );
        assert!(
            frame_text(&initial).contains("pi needs attention"),
            "expected initial full frame to include toast text"
        );

        let toast_row = server.app.state.view.toast_hit_area.y;
        let inner_rect = server.app.state.view.pane_infos[0].inner_rect;
        let pane_row = toast_row
            .checked_sub(inner_rect.y)
            .expect("toast should overlap the pane")
            + 1;
        assert!(pane_row <= inner_rect.height);
        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(format!("\x1b[{pane_row};1Hzzzz").as_bytes());

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "retained path should not stream a frame that can overwrite toast cells"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_declines_while_copy_feedback_is_visible() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.copy_feedback = Some(crate::app::state::CopyFeedback {
            message: "copied to clipboard".to_owned(),
        });
        server.render_and_stream();
        let initial = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial frame"),
        );
        let initial_text = frame_text(&initial);
        assert!(
            initial_text.contains("copied to clipboard"),
            "expected initial full frame to include copy feedback"
        );

        let feedback_row = initial_text
            .lines()
            .position(|line| line.contains("copied to clipboard"))
            .expect("copy feedback row") as u16;
        let inner_rect = server.app.state.view.pane_infos[0].inner_rect;
        let pane_row = feedback_row
            .checked_sub(inner_rect.y)
            .expect("copy feedback should overlap the pane")
            + 1;
        assert!(pane_row <= inner_rect.height);
        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(format!("\x1b[{pane_row};1Hzzzz").as_bytes());

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "retained path should not stream a frame that can overwrite copy feedback cells"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_matches_full_render_frame() {
        let initial = b"\x1b[6 qleft \xe4\xb8\xad";
        let update = b"\r\x1b[44mZ\x1b[0m";
        let (mut retained_server, retained_rx, retained_pane_id) = retained_test_server(initial);
        let (mut full_server, full_rx, full_pane_id) = retained_test_server(initial);

        retained_server.render_and_stream();
        let _ = retained_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial retained baseline");
        full_server.render_and_stream();
        let _ = full_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial full baseline");

        retained_server
            .app
            .state
            .runtime_for_pane_in_workspace(
                &retained_server.app.terminal_runtimes,
                0,
                retained_pane_id,
            )
            .expect("retained runtime")
            .test_process_pty_bytes(update);
        full_server
            .app
            .state
            .runtime_for_pane_in_workspace(&full_server.app.terminal_runtimes, 0, full_pane_id)
            .expect("full runtime")
            .test_process_pty_bytes(update);

        assert!(retained_server.render_retained_pty_update_and_stream());
        full_server.render_and_stream();

        let retained_frame = read_server_frame(
            retained_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame"),
        );
        let full_frame = read_server_frame(
            full_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("full frame"),
        );
        assert_frame_data_eq(&retained_frame, &full_frame);
    }

    #[tokio::test]
    async fn retained_pty_update_streams_cursor_only_change() {
        let initial = b"abcd";
        let update = b"\x1b[D";
        let (mut retained_server, retained_rx, retained_pane_id) = retained_test_server(initial);
        let (mut full_server, full_rx, full_pane_id) = retained_test_server(initial);

        retained_server.render_and_stream();
        let _ = retained_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial retained baseline");
        full_server.render_and_stream();
        let _ = full_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial full baseline");

        retained_server
            .app
            .state
            .runtime_for_pane_in_workspace(
                &retained_server.app.terminal_runtimes,
                0,
                retained_pane_id,
            )
            .expect("retained runtime")
            .test_process_pty_bytes(update);
        full_server
            .app
            .state
            .runtime_for_pane_in_workspace(&full_server.app.terminal_runtimes, 0, full_pane_id)
            .expect("full runtime")
            .test_process_pty_bytes(update);

        assert!(retained_server.render_retained_pty_update_and_stream());
        full_server.render_and_stream();

        let retained_frame = read_server_frame(
            retained_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained cursor frame"),
        );
        let full_frame = read_server_frame(
            full_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("full cursor frame"),
        );
        assert_frame_data_eq(&retained_frame, &full_frame);
    }

    #[tokio::test]
    async fn retained_pty_update_declines_unsafe_mode_without_consuming_dirty_rows() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        server.app.state.set_server_mode(crate::app::Mode::Navigate);
        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());

        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        assert!(server.render_retained_pty_update_and_stream());
        let patched = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame after safe mode"),
        );
        assert!(patched.cells.iter().any(|cell| cell.symbol == "Z"));
    }

    #[tokio::test]
    async fn headless_full_render_clears_full_redraw_pending_for_future_retained_updates() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.full_redraw_pending = true;

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("full redraw frame");
        assert!(!server.app.full_redraw_pending);

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(server.render_retained_pty_update_and_stream());
    }

    #[tokio::test]
    async fn retained_pty_update_declines_when_patch_would_stale_hyperlinks() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"link");
        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");
        let inner_rect = server.app.state.view.pane_infos[0].inner_rect;
        let client = server.clients.get_mut(&1).unwrap();
        let mut frame = client.render_state.last_frame().unwrap().clone();
        frame.hyperlinks = vec!["https://example.com".to_owned()];
        let hyperlink_idx =
            usize::from(inner_rect.y) * usize::from(frame.width) + usize::from(inner_rect.x);
        frame.cells[hyperlink_idx].hyperlink = Some(0);
        let prepared = client
            .render_state
            .prepare_frame(frame)
            .expect("hyperlink frame differs");
        client.render_state.commit_sent_frame(prepared);

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rplain");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());

        server.render_and_stream();
        let full = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("full frame after hyperlink overwrite"),
        );
        assert!(
            full.cells.iter().all(|cell| cell.hyperlink.is_none()),
            "full render should clear overwritten hyperlink cells"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_allows_dirty_row_that_creates_plain_url() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"plain");
        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rhttps://example.com/new");

        assert!(server.render_retained_pty_update_and_stream());
        let patched = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame after plain URL"),
        );
        assert!(
            patched.hyperlinks.is_empty(),
            "retained render should not synthesize plain URL hyperlink metadata"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_allows_kitty_enabled_empty_graphics_cache() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.kitty_graphics_enabled = true;
        server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(server.render_retained_pty_update_and_stream());
        let retained = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame with kitty enabled"),
        );
        assert!(retained.cells.iter().any(|cell| cell.symbol == "Z"));
    }

    #[tokio::test]
    async fn retained_pty_update_declines_when_graphics_cache_has_content() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.kitty_graphics_enabled = true;
        let client = server.clients.get_mut(&1).unwrap();
        client.cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");
        server
            .clients
            .get_mut(&1)
            .unwrap()
            .graphics_cache
            .test_mark_non_empty();

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[tokio::test]
    async fn full_redraw_pending_survives_full_render_queue_full() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        server.clients[&1]
            .writer
            .as_ref()
            .unwrap()
            .test_fill_render(queued);
        server.app.full_redraw_pending = true;

        server.render_and_stream();

        assert!(server.app.full_redraw_pending);
        assert_eq!(
            server.clients.get(&1).unwrap().deferred_render(),
            DeferredRender::Full
        );
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn client_config_reload_request_refreshes_attached_clients() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.app.state.request_client_config_reload = true;

        server.drain_client_config_reload_request();

        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("client config reload message"),
        ) {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected ReloadSoundConfig, got {other:?}"),
        }
        assert!(!server.app.state.request_client_config_reload);
    }

    #[test]
    fn notification_bell_updates_the_initiating_clients_local_effective_config() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-remote-notification-toggle-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("temp config directory");
        let server_path = directory.join("server.toml");
        let client_path = directory.join("client.toml");
        std::fs::write(&server_path, "# server config\n").expect("seed server config");
        std::fs::write(
            &client_path,
            "[ui.toast]\nterminal_backend = \"osc99\"\n[ui.sound]\nenabled = false\n",
        )
        .expect("seed client config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &server_path);

        let mut server = test_headless_server();
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.sidebar_collapsed = false;
        crate::ui::compute_view(
            &mut server.app.state,
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );
        let bell = server.app.state.view.notification_hit_area;
        assert_eq!(bell.width, 2);

        let (initiator_tx, initiator_control_rx, _initiator_render_rx) = test_client_writer();
        let (other_tx, other_control_rx, _other_render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                Some(initiator_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                2,
                RenderEncoding::SemanticFrame,
                Some(other_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let click_bell = |server: &mut HeadlessServer| {
            assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
                client_id: 1,
                events: vec![crate::protocol::ClientInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Down(
                        crate::protocol::ClientMouseButton::Left,
                    ),
                    column: bell.x,
                    row: bell.y,
                    modifiers: 0,
                }],
            }));
            server.drain_client_config_reload_request();
        };

        let mut apply_client_message = |message: ServerMessage| {
            let ServerMessage::NotificationConfig { sound_enabled } = message else {
                panic!("expected initiating client notification config update, got {message:?}");
            };
            env.set(crate::config::CONFIG_PATH_ENV_VAR, &client_path);
            let mut sound = crate::config::SoundConfig::default();
            let mut backend = crate::config::TerminalNotificationBackend::Auto;
            crate::client::reload_local_notification_config(
                &mut sound,
                &mut backend,
                sound_enabled,
            );
            env.set(crate::config::CONFIG_PATH_ENV_VAR, &server_path);
            (sound, backend)
        };

        click_bell(&mut server);
        let (client_sound, client_backend) = apply_client_message(read_server_message(
            initiator_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initiating client config update"),
        ));
        let saved = crate::config::Config::load().config;
        assert_eq!(
            saved.ui.toast.delivery,
            crate::config::ToastDelivery::Terminal
        );
        assert!(saved.ui.sound.enabled);
        assert!(client_sound.enabled, "initiating client stayed muted");
        assert_eq!(
            client_backend,
            crate::config::TerminalNotificationBackend::Osc99
        );
        assert!(
            other_control_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "notification toggle reached a non-initiating client"
        );

        click_bell(&mut server);
        let (client_sound, client_backend) = apply_client_message(read_server_message(
            initiator_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initiating client config update"),
        ));
        let saved = crate::config::Config::load().config;
        assert_eq!(saved.ui.toast.delivery, crate::config::ToastDelivery::Off);
        assert!(!saved.ui.sound.enabled);
        assert!(!client_sound.enabled);
        assert_eq!(
            client_backend,
            crate::config::TerminalNotificationBackend::Osc99
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn sections_sidebar_area_checklist_uses_headless_client_hit_areas() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-headless-sidebar-areas-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("temp config directory");
        let config_path = directory.join("config.toml");
        std::fs::write(
            &config_path,
            "[ui.sidebar]\nlayout = \"sections\"\n[ui.sidebar.areas]\nnotes = true\n",
        )
        .expect("seed sections config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &config_path);

        let mut server = test_headless_server();
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.sidebar_collapsed = false;
        server.app.state.sidebar_sections_layout = true;
        crate::ui::compute_view(
            &mut server.app.state,
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );
        let checklist = server.app.state.view.sidebar_areas_hit_area;
        let bell = server.app.state.view.notification_hit_area;
        let cycle = server.app.state.view.window_cycle_mode_hit_area;
        assert_eq!(checklist.width, 1);
        assert_eq!(checklist.y, bell.y);
        assert_eq!(checklist.x, 16);
        assert!(checklist.right() < bell.x);
        assert!(cycle.right() <= checklist.x);

        let (writer, _control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                Some(writer),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let click = |server: &mut HeadlessServer, x, y| {
            assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
                client_id: 1,
                events: vec![crate::protocol::ClientInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Down(
                        crate::protocol::ClientMouseButton::Left,
                    ),
                    column: x,
                    row: y,
                    modifiers: 0,
                }],
            }));
        };

        click(&mut server, checklist.x, checklist.y);
        assert_eq!(server.app.state.sidebar_areas_menu_selected, Some(0));
        let menu = crate::ui::sidebar::sidebar_areas_menu_layout(
            &server.app.state,
            server.app.state.screen_rect(),
        )
        .expect("checklist menu layout");
        click(&mut server, menu.list_rect.x, menu.list_rect.y + 6);
        assert!(!server.app.state.sidebar_areas.notes);
        assert!(!crate::config::Config::load().config.ui.sidebar.areas.notes);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn notification_permission_survives_reconnect_after_bell_toggle() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-notification-reconnect-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("temp config directory");
        let server_path = directory.join("server.toml");
        let client_path = directory.join("client.toml");
        std::fs::write(&server_path, "# server config\n").expect("seed server config");
        std::fs::write(
            &client_path,
            "[ui.toast]\nterminal_backend = \"osc99\"\n[ui.sound]\nenabled = false\n",
        )
        .expect("seed client config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &server_path);

        let mut server = test_headless_server();
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.sidebar_collapsed = false;
        crate::ui::compute_view(
            &mut server.app.state,
            ratatui::layout::Rect::new(0, 0, 80, 24),
        );
        let bell = server.app.state.view.notification_hit_area;
        assert_eq!(bell.width, 2);

        let (writer, control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            keybindings: None,
            writer,
            render_encoding: RenderEncoding::SemanticFrame,
            direct_attach_requested: false,
            direct_graphics: false,
        }));
        assert!(matches!(
            read_server_message(
                control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("muted notification config on initial attach")
            ),
            ServerMessage::NotificationConfig {
                sound_enabled: false
            }
        ));
        assert!(matches!(
            read_server_message(
                control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute input-source state on initial attach")
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Down(
                    crate::protocol::ClientMouseButton::Left,
                ),
                column: bell.x,
                row: bell.y,
                modifiers: 0,
            }],
        }));
        server.drain_client_config_reload_request();
        assert!(matches!(
            read_server_message(
                control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("notification config after enabling bell")
            ),
            ServerMessage::NotificationConfig {
                sound_enabled: true
            }
        ));

        assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 1 }));
        let (writer, control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            keybindings: None,
            writer,
            render_encoding: RenderEncoding::SemanticFrame,
            direct_attach_requested: false,
            direct_graphics: false,
        }));

        let reconnect_config = read_server_message(
            control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("server notification config on reconnect"),
        );
        let ServerMessage::NotificationConfig { sound_enabled } = reconnect_config else {
            panic!("expected reconnect notification config, got {reconnect_config:?}");
        };
        assert!(matches!(
            read_server_message(
                control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("absolute input-source state on reconnect")
            ),
            ServerMessage::PrefixInputSource { active: false }
        ));
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &client_path);
        let mut client_sound = crate::config::SoundConfig::default();
        let mut client_backend = crate::config::TerminalNotificationBackend::Auto;
        crate::client::reload_local_notification_config(
            &mut client_sound,
            &mut client_backend,
            sound_enabled,
        );
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &server_path);
        assert!(client_sound.enabled, "reconnected client stayed muted");
        assert_eq!(
            client_backend,
            crate::config::TerminalNotificationBackend::Osc99,
            "server permission must not replace the client's terminal backend"
        );

        server.app.state.request_client_config_reload = true;
        server.drain_client_config_reload_request();
        assert!(matches!(
            read_server_message(
                control_rx
                    .recv_timeout(Duration::from_millis(100))
                    .expect("local config reload request")
            ),
            ServerMessage::ReloadSoundConfig
        ));
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &client_path);
        let local_config = crate::config::Config::load().config;
        client_sound = local_config.ui.sound;
        client_backend = local_config.ui.toast.terminal_backend;
        assert!(
            !client_sound.enabled,
            "client fixture must reproduce the local config reset"
        );
        let restored = read_server_message(
            control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("server notification config after local config reload"),
        );
        let ServerMessage::NotificationConfig { sound_enabled } = restored else {
            panic!("expected restored notification config, got {restored:?}");
        };
        crate::client::reload_local_notification_config(
            &mut client_sound,
            &mut client_backend,
            sound_enabled,
        );
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &server_path);
        assert!(
            client_sound.enabled,
            "local config reload reverted the server's effective permission"
        );
        assert_eq!(
            client_backend,
            crate::config::TerminalNotificationBackend::Osc99
        );

        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "notify-after-reconnect".into(),
                    method: api::schema::Method::NotificationShow(
                        api::schema::NotificationShowParams {
                            title: "build finished".into(),
                            body: None,
                            position: None,
                            sound: api::schema::NotificationShowSound::Done,
                        },
                    ),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("notification response");
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        let delivered = read_server_message(
            control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("sound notification after reconnect"),
        );
        assert!(
            matches!(
                delivered,
                ServerMessage::Notify {
                    kind: protocol::NotifyKind::Sound,
                    message,
                    body: None,
                } if message == "agent done"
            ) && client_sound.enabled,
            "client must play the delivered sound after reconnect"
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn terminal_bell_targets_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_control_rx, _background_rx) = test_client_writer();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(background_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(2);

        let changed = server.handle_internal_event_with_forwarding(AppEvent::TerminalBell {
            pane_id: crate::layout::PaneId::from_raw(1),
            count: 3,
        });

        assert!(!changed);
        match read_server_message(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground terminal bell message"),
        ) {
            ServerMessage::TerminalBell { count } => assert_eq!(count, 3),
            other => panic!("expected terminal bell message, got {other:?}"),
        }
        assert!(
            background_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive terminal bells"
        );

        server.foreground_client_id = None;
        server.handle_internal_event_with_forwarding(AppEvent::TerminalBell {
            pane_id: crate::layout::PaneId::from_raw(1),
            count: 1,
        });
        assert!(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "bells without a foreground client must not be retained"
        );
    }

    #[test]
    fn clipboard_write_targets_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_control_rx, _background_rx) = test_client_writer();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(background_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
            content: b"test".to_vec(),
        });

        assert!(changed);
        assert_eq!(
            server
                .app
                .state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("copied to clipboard")
        );
        match read_server_message(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground clipboard message"),
        ) {
            ServerMessage::Clipboard { data } => assert_eq!(data, "dGVzdA=="),
            other => panic!("expected clipboard message, got {other:?}"),
        }
        assert!(
            background_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive clipboard writes"
        );
    }

    #[test]
    fn clipboard_write_without_foreground_client_does_not_show_feedback() {
        let mut server = test_headless_server();
        server.foreground_client_id = None;

        let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
            content: b"test".to_vec(),
        });

        assert!(changed);
        assert!(
            server.app.state.copy_feedback.is_none(),
            "clipboard feedback should only show when a foreground client can receive the write"
        );
    }

    #[test]
    fn clipboard_write_failed_foreground_send_does_not_show_feedback() {
        let mut server = test_headless_server();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();
        drop(foreground_control_rx);
        foreground_tx.test_close();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
            content: b"test".to_vec(),
        });

        assert!(changed);
        assert!(
            server.app.state.copy_feedback.is_none(),
            "clipboard feedback should only show after the foreground client receives the write"
        );
        assert!(
            !server.clients.contains_key(&1),
            "failed targeted send should remove the broken foreground client"
        );
    }

    #[test]
    fn prefix_input_source_targets_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_control_rx, _background_rx) = test_client_writer();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(background_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();
        // Drain any setup messages (e.g. mouse-capture sync) before exercising the event.
        while foreground_control_rx
            .recv_timeout(Duration::from_millis(20))
            .is_ok()
        {}

        let changed = server.handle_internal_event_with_forwarding(AppEvent::PrefixInputSource {
            client_id: None,
            active: true,
        });

        assert!(changed);
        match read_server_message(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground prefix input-source message"),
        ) {
            ServerMessage::PrefixInputSource { active } => assert!(active),
            other => panic!("expected prefix input-source message, got {other:?}"),
        }
        assert!(
            background_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive prefix input-source changes"
        );
    }

    #[test]
    fn headless_app_keeps_prefix_input_source_switch_off_process() {
        // An App-internal drain (e.g. the exhaustive drain at the top of
        // handle_api_request) can consume a queued PrefixInputSource intent
        // before the forwarding drain sees it. The headless App must treat the
        // event as inert instead of switching the host input source from the
        // server process.
        struct CountingPrefixInputSource(std::rc::Rc<std::cell::Cell<usize>>);
        impl crate::platform::PrefixInputSource for CountingPrefixInputSource {
            fn switch_to_ascii(&mut self) {
                self.0.set(self.0.get() + 1);
            }
            fn restore(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let mut server = test_headless_server();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        server
            .app
            .set_prefix_input_source(Box::new(CountingPrefixInputSource(calls.clone())));

        server
            .app
            .handle_internal_event(AppEvent::PrefixInputSource {
                client_id: None,
                active: true,
            });
        server
            .app
            .handle_internal_event(AppEvent::PrefixInputSource {
                client_id: None,
                active: false,
            });
        assert_eq!(
            calls.get(),
            0,
            "headless server must not apply the host input-source switch"
        );

        // Sanity: the same event does apply once the flag is on (monolithic semantics).
        server.app.local_input_source_switch = true;
        server
            .app
            .handle_internal_event(AppEvent::PrefixInputSource {
                client_id: None,
                active: true,
            });
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn client_local_notifications_target_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_control_rx, _background_rx) = test_client_writer();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(background_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(server.send_to_foreground_client(ServerMessage::Notify {
            kind: protocol::NotifyKind::Toast,
            message: "pi finished".to_string(),
            body: Some("workspace 1".to_string()),
        }));

        match read_server_message(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground toast message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Toast);
                assert_eq!(message, "pi finished");
                assert_eq!(body.as_deref(), Some("workspace 1"));
            }
            other => panic!("expected toast notify, got {other:?}"),
        }
        assert!(
            background_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive client-local notifications"
        );
    }

    #[test]
    fn oversized_paste_rejection_notifies_only_the_sending_client() {
        let mut server = test_headless_server();
        let (sender_writer, sender_control_rx, _sender_render_rx) = test_client_writer();
        let (foreground_writer, foreground_control_rx, _foreground_render_rx) =
            test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(sender_writer),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(foreground_writer),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(
            !server.handle_server_event(ServerEvent::ClientPasteRejected {
                client_id: 1,
                size: 5_000_012,
                max: 1_048_576,
            })
        );

        match read_server_message(
            sender_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("sending client rejection notification"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Toast);
                assert_eq!(message, "Paste rejected");
                assert_eq!(
                    body.as_deref(),
                    Some("Input message is 5000012 bytes; Herdr's limit is 1048576 bytes")
                );
            }
            other => panic!("expected paste rejection notification, got {other:?}"),
        }
        assert!(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "foreground client must not receive another client's rejection"
        );
        assert_eq!(server.foreground_client_id, Some(2));
        assert_eq!(server.clients.len(), 2);
        assert!(server.app.state.toast.is_none());
    }

    #[test]
    fn herdr_toast_delivery_keeps_toast_in_frame_without_client_notify() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let changed = server.handle_internal_event_with_forwarding(AppEvent::UpdateReady {
            version: "9.9.9".to_string(),
            install_command: "herdr update".into(),
        });

        assert!(changed);
        assert!(server.app.state.toast.is_some());
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "herdr delivery should render in-frame instead of forwarding a client-local notification"
        );
    }

    #[test]
    fn system_toast_delivery_forwards_system_notify_kind() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let changed = server.handle_internal_event_with_forwarding(AppEvent::UpdateReady {
            version: "9.9.9".to_string(),
            install_command: "herdr update".into(),
        });

        assert!(changed);
        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("system toast message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "v9.9.9 available");
                assert_eq!(
                    body.as_deref(),
                    Some("detach, run `herdr update`, then follow its restart guidance")
                );
            }
            other => panic!("expected system toast notify, got {other:?}"),
        }
    }

    #[test]
    fn notification_show_api_forwards_system_notification_to_foreground_client() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: Some("api workspace".into()),
                        position: Some(crate::config::ToastHerdrPosition::TopLeft),
                        sound: api::schema::NotificationShowSound::Request,
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        let first = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api notification message"),
        );
        let second = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api sound message"),
        );

        match first {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "build failed");
                assert_eq!(body.as_deref(), Some("api workspace"));
            }
            other => panic!("expected api notification, got {other:?}"),
        }
        match second {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Sound);
                assert_eq!(message, "agent attention");
                assert!(body.is_none());
            }
            other => panic!("expected api sound, got {other:?}"),
        }
    }

    #[test]
    fn notification_show_api_preserves_colon_in_forwarded_title() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "build: failed".into(),
                        body: Some("api workspace".into()),
                        position: None,
                        sound: api::schema::NotificationShowSound::None,
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api notification message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "build: failed");
                assert_eq!(body.as_deref(), Some("api workspace"));
            }
            other => panic!("expected api notification, got {other:?}"),
        }
    }

    #[test]
    fn notification_show_api_validates_empty_title_before_disabled_delivery() {
        let mut server = test_headless_server();
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Off;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "\n\t".into(),
                        body: None,
                        position: None,
                        sound: api::schema::NotificationShowSound::None,
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.error.code, "invalid_params");
        assert_eq!(parsed.error.message, "notification title is empty");
    }

    #[test]
    fn notification_show_api_reports_no_foreground_client() {
        let mut server = test_headless_server();
        server.foreground_client_id = None;
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: api::schema::NotificationShowSound::Request,
                    },
                ),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: api::schema::NotificationShowReason::NoForegroundClient,
            }
        );
    }

    #[test]
    fn notification_show_api_herdr_toast_expires_headless() {
        let mut server = test_headless_server();
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "notify".into(),
                    method: api::schema::Method::NotificationShow(
                        api::schema::NotificationShowParams {
                            title: "build failed".into(),
                            body: None,
                            position: None,
                            sound: api::schema::NotificationShowSound::None,
                        },
                    ),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );

        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        let deadline = server.app.toast_deadline.expect("api toast deadline");
        assert!(server.handle_scheduled_tasks_headless(deadline, false));
        assert!(server.app.state.toast.is_none());
        assert!(server.app.toast_deadline.is_none());
    }

    #[test]
    fn notification_show_api_forwards_sound_for_herdr_delivery() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "notify".into(),
                    method: api::schema::Method::NotificationShow(
                        api::schema::NotificationShowParams {
                            title: "build failed".into(),
                            body: None,
                            position: None,
                            sound: api::schema::NotificationShowSound::Done,
                        },
                    ),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );

        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api sound message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Sound);
                assert_eq!(message, "agent done");
                assert!(body.is_none());
            }
            other => panic!("expected api sound, got {other:?}"),
        }
    }

    #[test]
    fn startup_idle_does_not_forward_completion() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("active");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;
        server.app.state.toast_config.delay_seconds = 0;
        server.app.state.sound.enabled = true;

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::AgentProcessDetected {
                pane_id,
                agent: crate::detect::Agent::Pi,
                observed_at: Instant::now(),
            })
        );

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        while client_control_rx
            .recv_timeout(Duration::from_millis(20))
            .is_ok()
        {}

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
                pane_id,
                agent: Some(crate::detect::Agent::Pi),
                state: crate::detect::AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                process_exited: false,
                observed_at: Instant::now(),
                usage_limited: false,
            })
        );
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "startup readiness should not forward a completion notification"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn headless_agent_prompt_does_not_notify_when_it_retires_a_blocked_closing_gate() {
        let mut server = test_headless_server();
        let background = crate::workspace::Workspace::test_new("background");
        let pane_id = background.tabs[0].root_pane;
        let public_pane_id = format!("{}:p1", background.id);
        let foreground = crate::workspace::Workspace::test_new("foreground");
        server.app.state.workspaces = vec![background, foreground];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(1);
        server.app.state.selected = 1;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Terminal;
        server.app.state.toast_config.delay_seconds = 0;
        server.app.state.sound.enabled = true;

        let terminal_id = server.app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("pane terminal");
        let terminal = server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Codex),
            crate::detect::AgentState::Idle,
        );
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            crate::detect::AgentState::Blocked,
            None,
            Some(1),
        );
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "prompt".into(),
                    method: api::schema::Method::AgentPrompt(api::schema::AgentPromptParams {
                        target: public_pane_id,
                        text: "continue".into(),
                        wait: None,
                    }),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );

        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("agent.prompt response");
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            parsed.result,
            api::schema::ResponseResult::AgentPrompted { .. }
        ));
        assert_eq!(
            server.app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "retiring the input gate must not forward a Finished toast or done sound"
        );
    }

    #[test]
    fn headless_api_genuine_completion_still_forwards_finished_toast_and_done_sound() {
        let mut server = test_headless_server();
        let background = crate::workspace::Workspace::test_new("background");
        let pane_id = background.tabs[0].root_pane;
        let public_pane_id = format!("{}:p1", background.id);
        let foreground = crate::workspace::Workspace::test_new("foreground");
        server.app.state.workspaces = vec![background, foreground];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(1);
        server.app.state.selected = 1;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Terminal;
        server.app.state.toast_config.delay_seconds = 0;
        server.app.state.sound.enabled = true;

        let terminal_id = server.app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("pane terminal");
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Idle,
            );
        let session_path = std::env::current_dir()
            .unwrap()
            .join("headless-pi-completion.jsonl")
            .display()
            .to_string();
        let session_ref = crate::agent_resume::AgentSessionRef::path(&session_path).unwrap();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:pi".into(),
                agent: "pi".into(),
                session_ref: session_ref.clone(),
            });
        assert!(server
            .app
            .handle_internal_event(AppEvent::HookStateReported {
                pane_id,
                source: "herdr:pi".into(),
                agent_label: "pi".into(),
                state: crate::detect::AgentState::Working,
                message: None,
                seq: Some(20),
                wait: None,
                eta_s: None,
                reported_at: None,
                session_ref: Some(session_ref),
                closing_block: None,
            })
            .unwrap_or(false));
        assert_eq!(
            server.app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Working
        );

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "complete".into(),
                    method: api::schema::Method::PaneReportAgent(
                        api::schema::PaneReportAgentParams {
                            pane_id: public_pane_id,
                            source: "herdr:pi".into(),
                            agent: "pi".into(),
                            state: api::schema::PaneAgentState::Idle,
                            v: None,
                            message: None,
                            seq: Some(21),
                            wait: None,
                            eta_s: None,
                            reported_at: None,
                            agent_session_id: None,
                            agent_session_path: Some(session_path),
                            gates: None,
                            items: None,
                            decisions: None,
                            completion: None,
                            external_wait: None,
                            parse_status: None,
                            workers_unknown: None,
                            agents: None,
                        },
                    ),
                },
                respond_to,
                response_write_complete: None,
                stream_active: None,
            })
        );
        assert!(response_rx.recv_timeout(Duration::from_millis(100)).is_ok());
        assert_eq!(
            server.app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );

        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("Finished toast"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Toast);
                assert_eq!(message, "pi finished");
                assert_eq!(body.as_deref(), Some("background · 1"));
            }
            other => panic!("expected Finished toast, got {other:?}"),
        }
        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("done sound"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Sound);
                assert_eq!(message, "agent done");
                assert!(body.is_none());
            }
            other => panic!("expected done sound, got {other:?}"),
        }
    }

    #[test]
    fn delayed_agent_notification_forwards_after_deadline() {
        let mut server = test_headless_server();
        let background = crate::workspace::Workspace::test_new("background");
        let pane_id = background.tabs[0].root_pane;
        let foreground = crate::workspace::Workspace::test_new("foreground");
        server.app.state.workspaces = vec![background, foreground];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(1);
        server.app.state.selected = 1;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;
        server.app.state.toast_config.delay_seconds = 1;
        server.app.state.sound.enabled = true;

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let changed = server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
            pane_id,
            agent: Some(crate::detect::Agent::Pi),
            state: crate::detect::AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: Instant::now(),
        });

        assert!(changed);
        assert!(server.app.state.toast.is_none());
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "delayed transition should not notify immediately"
        );

        let deadline = server
            .app
            .state
            .next_pending_agent_notification_deadline()
            .expect("pending notification deadline");
        assert!(server.handle_scheduled_tasks_headless(deadline, false));

        let first = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed sound message"),
        );
        let second = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed toast message"),
        );

        assert!(matches!(
            first,
            ServerMessage::Notify {
                kind: protocol::NotifyKind::Sound,
                ..
            }
        ));
        match second {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "pi needs attention");
                assert_eq!(body.as_deref(), Some("background · 1"));
            }
            other => panic!("expected delayed system toast, got {other:?}"),
        }
        assert!(server.app.state.pending_agent_notifications.is_empty());
    }

    #[test]
    fn delayed_active_tab_unfocused_agent_notification_forwards_after_deadline() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("active");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;
        server.app.state.toast_config.delay_seconds = 1;
        server.app.state.sound.enabled = true;

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
                pane_id,
                agent: Some(crate::detect::Agent::Pi),
                state: crate::detect::AgentState::Blocked,
                visible_blocker: false,
                visible_working: false,
                usage_limited: false,
                process_exited: false,
                observed_at: Instant::now(),
            })
        );
        assert!(server.app.state.toast.is_none());
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "delayed transition should not notify immediately"
        );

        let deadline = server
            .app
            .state
            .next_pending_agent_notification_deadline()
            .expect("pending notification deadline");
        assert!(server.handle_scheduled_tasks_headless(deadline, false));

        let first = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed sound message"),
        );
        let second = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed toast message"),
        );

        assert!(matches!(
            first,
            ServerMessage::Notify {
                kind: protocol::NotifyKind::Sound,
                ..
            }
        ));
        match second {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "pi needs attention");
                assert_eq!(body.as_deref(), Some("active · 1"));
            }
            other => panic!("expected delayed system toast, got {other:?}"),
        }
    }

    #[test]
    fn stale_api_agent_report_does_not_forward_done_sound() {
        let mut server = test_headless_server();
        let background = crate::workspace::Workspace::test_new("background");
        let pane_id = background.tabs[0].root_pane;
        let public_pane_id = format!("{}:p1", background.id);
        let foreground = crate::workspace::Workspace::test_new("foreground");
        server.app.state.workspaces = vec![background, foreground];
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Idle,
            );
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:pi".into(),
                agent: "pi".into(),
                session_ref: crate::agent_resume::AgentSessionRef::path(
                    std::env::current_dir()
                        .unwrap()
                        .join("headless-pi-session.jsonl")
                        .display()
                        .to_string(),
                )
                .unwrap(),
            });
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority(
                "herdr:pi".into(),
                "pi".into(),
                crate::detect::AgentState::Working,
                None,
                Some(20),
            );
        server.app.state.active = Some(1);
        server.app.state.selected = 1;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "stale".into(),
                method: api::schema::Method::PaneReportAgent(api::schema::PaneReportAgentParams {
                    pane_id: public_pane_id,
                    source: "herdr:pi".into(),
                    agent: "pi".into(),
                    state: api::schema::PaneAgentState::Idle,
                    v: None,
                    message: None,
                    seq: Some(19),
                    wait: None,
                    eta_s: None,
                    reported_at: None,
                    agent_session_id: None,
                    agent_session_path: None,
                    gates: None,
                    items: None,
                    decisions: None,
                    completion: None,
                    external_wait: None,
                    parse_status: None,
                    workers_unknown: None,
                    agents: None,
                }),
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        });

        assert!(changed);
        assert!(response_rx.recv_timeout(Duration::from_millis(100)).is_ok());
        assert_eq!(
            server
                .app
                .state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .raw_agent_state(),
            crate::detect::AgentState::Working
        );
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "stale idle report must not forward a done sound"
        );
    }

    #[test]
    fn ac6_focused_work_context_does_not_rename_tab_or_outer_client() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("agent-fleet");
        workspace.tabs[0].set_custom_name("manual-title".into());
        let pane_id = workspace.tabs[0].root_pane;
        let public_pane_id = format!("{}:p1", workspace.id);
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.set_server_mode(crate::app::Mode::Terminal);

        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let session_ref = crate::agent_resume::AgentSessionRef::id("session-1").unwrap();
        let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Codex),
            crate::detect::AgentState::Working,
        );
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: session_ref.clone(),
        });
        terminal.set_hook_authority_with_session_ref(
            "herdr:codex".into(),
            "codex".into(),
            crate::detect::AgentState::Working,
            None,
            Some(session_ref),
            None,
        );

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let report = |title: &str, seq| {
            api::schema::Method::PaneReportMetadata(api::schema::PaneReportMetadataParams {
                pane_id: public_pane_id.clone(),
                source: crate::work_title::WORK_TITLE_SOURCE.into(),
                agent: Some("codex".into()),
                applies_to_source: Some("herdr:codex".into()),
                agent_session_id: Some("session-1".into()),
                title: Some(title.into()),
                work_context: None,
                work_title_long_brief: false,
                display_agent: None,
                state_labels: HashMap::new(),
                tokens: HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                seq: Some(seq),
                ttl_ms: None,
            })
        };
        let send = |server: &mut HeadlessServer, method| {
            let (respond_to, response_rx) = std::sync::mpsc::channel();
            assert!(
                server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                    request: api::schema::Request {
                        id: "work-title".into(),
                        method,
                    },
                    respond_to,
                    response_write_complete: None,
                    stream_active: None,
                })
            );
            let response = response_rx
                .recv_timeout(Duration::from_millis(100))
                .unwrap();
            let _: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        };

        send(&mut server, report("Fix Billing Retry Regression", 20));
        assert_eq!(
            server.app.state.workspaces[0].tabs[0]
                .custom_name
                .as_deref(),
            Some("manual-title")
        );
        assert_eq!(
            server.app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "work context must not update the outer client title"
        );

        send(&mut server, report("Review Auth Migration Safety", 22));
        assert_eq!(
            server.app.state.workspaces[0].tabs[0]
                .custom_name
                .as_deref(),
            Some("manual-title")
        );
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "changed work context must not update the outer client title"
        );

        send(&mut server, report("Overwrite Newer Work Title", 19));
        assert_eq!(
            server.app.state.workspaces[0].tabs[0]
                .custom_name
                .as_deref(),
            Some("manual-title")
        );
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "stale turn must not overwrite either title surface"
        );
    }

    /// Verify that no direct calls to `self.app.handle_internal_event`
    /// (or its `handle_internal_event_with_prefix_sync` wrapper) exist
    /// outside of `handle_internal_event_with_forwarding` in this
    /// module. This ensures the forwarding bypass cannot be reintroduced.
    ///
    /// The search pattern looks for `handle_internal_event` calls that
    /// are NOT inside the `handle_internal_event_with_forwarding` method.
    #[test]
    fn no_handle_internal_event_bypass_in_module() {
        let source = include_str!("headless.rs");

        // Find all lines containing handle_internal_event
        let mut bypass_lines: Vec<String> = Vec::new();
        let mut inside_forwarding_method = false;
        let mut forwarding_method_brace_depth = 0u32;

        for (i, line) in source.lines().enumerate() {
            let line_num = i + 1;

            // Track when we're inside handle_internal_event_with_forwarding
            if line.contains("fn handle_internal_event_with_forwarding") {
                inside_forwarding_method = true;
                forwarding_method_brace_depth = 0;
            }

            if inside_forwarding_method {
                // Count braces to track when we exit the method
                for ch in line.chars() {
                    match ch {
                        '{' => forwarding_method_brace_depth += 1,
                        '}' => {
                            forwarding_method_brace_depth =
                                forwarding_method_brace_depth.saturating_sub(1);
                            if forwarding_method_brace_depth == 0 {
                                inside_forwarding_method = false;
                            }
                        }
                        _ => {}
                    }
                }
            } else if (line.contains("self.app.handle_internal_event(")
                || line.contains("self.app.handle_internal_event_with_prefix_sync("))
                && !line.trim().starts_with("///")
                && !line.contains("contains(")
            {
                // Direct call to handle_internal_event outside the forwarding method
                bypass_lines.push(format!("line {}: {}", line_num, line.trim()));
            }
        }

        assert!(
            bypass_lines.is_empty(),
            "Found direct calls to self.app.handle_internal_event outside \
             handle_internal_event_with_forwarding (bypass risk):\n  {}",
            bypass_lines.join("\n  ")
        );
    }
}
