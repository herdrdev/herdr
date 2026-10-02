//! Application orchestration.
//!
//! - `state.rs` — AppState, Mode, and pure data structs
//! - `actions.rs` — state mutations (testable without PTYs/async)
//! - `input.rs` — key/mouse → action translation

pub(crate) mod actions;
mod add_project;
pub(crate) mod agent_model_log;
pub(crate) mod agent_resume;
pub(crate) mod agent_view;
mod agents;
pub(crate) mod auto_nudge;
pub(crate) use agents::{AGENT_START_SETTLE_DELAY, MAX_AGENT_START_TIMEOUT};
mod api;
mod api_helpers;
pub(crate) use api_helpers::{pane_agent_status_with_stale, read_terminal_snapshot};
pub(crate) mod claude_subagents;
mod command_palette;
pub(crate) use command_palette::PaletteEntry;
mod config_io;
mod creation;
pub(crate) mod diff;
mod files;
pub(crate) mod foreground_process;
mod git_actions;
mod git_refresh;
mod goals;
pub(crate) mod home;
pub(crate) mod home_catalog;
pub(crate) mod home_refs;
mod ids;
pub(crate) mod inbox;
mod input;
pub(crate) mod launch_profiles;
pub(crate) mod machines;
#[cfg(test)]
pub(crate) use input::SidebarWorkGroupKeyAction;
mod notepad;
pub(crate) mod pane_graphics;
mod pane_lifecycle;
pub(crate) use pane_lifecycle::pane_is_quiet;
mod pane_send;
mod popup;
pub(crate) mod probes;
pub(crate) mod projects;
pub(crate) mod remote_focus;
mod repo_editor;
mod repo_routing;
mod runtime;
mod runtime_mutations;
mod session;
pub(crate) mod settings_archive;
pub(crate) mod settings_general;
pub(crate) mod settings_keybindings;
pub(crate) mod settings_providers;
pub(crate) mod settled;
pub(crate) mod settled_view;
pub(crate) mod spawn_dock;
pub mod state;
pub(crate) mod status_log;
mod tab_bar_status;
mod terminal_targets;
mod terminal_titles;
mod theme_sync;
mod user_actions;
mod window_title;
pub(crate) mod work_context_git;
mod worktrees;

use std::collections::{HashMap, HashSet};
use std::future::pending;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(30);
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GIT_REMOTE_STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
const GIT_REPO_DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const GIT_REFRESH_TIMEOUT: Duration = Duration::from_secs(2);
const LOOP_RECEIPT_FALLBACK_INTERVAL: Duration = Duration::from_secs(30);
const AUTO_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const PENDING_AGENT_RESUME_THEME_WAIT: Duration = Duration::from_millis(750);
const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_secs(5);
const SIDEBAR_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
const PANE_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
const PANE_COPY_HIGHLIGHT_DURATION: Duration = Duration::from_millis(500);
const COPY_FEEDBACK_DURATION: Duration = Duration::from_secs(2);
pub(crate) const HOVER_TOOLTIP_DELAY: Duration = Duration::from_millis(400);

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute, terminal,
};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, Notify};
use tracing::info;

use crate::config::Config;
use crate::events::AppEvent;

pub use state::{AppState, DockSurface, Mode, ToastKind, ViewState};

pub(crate) fn load_plugin_manifest(
    path: &str,
    enabled: bool,
) -> Result<crate::api::schema::InstalledPluginInfo, (&'static str, String)> {
    api::plugins::load_plugin_manifest(path, enabled)
}

/// Full application: AppState + runtime concerns (event channels, async I/O).
#[derive(Debug, Clone)]
pub(crate) struct OverlayPaneState {
    ws_idx: usize,
    tab_idx: usize,
    previous_focus: crate::layout::PaneId,
    previous_zoomed: bool,
    temp_files: Vec<std::path::PathBuf>,
}

#[cfg(test)]
impl OverlayPaneState {
    pub(crate) fn test(
        ws_idx: usize,
        tab_idx: usize,
        previous_focus: crate::layout::PaneId,
        previous_zoomed: bool,
    ) -> Self {
        Self {
            ws_idx,
            tab_idx,
            previous_focus,
            previous_zoomed,
            temp_files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PaneClickState {
    pane_id: crate::layout::PaneId,
    viewport_row: u16,
    col: u16,
    at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GitRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) started_at: Instant,
    pub(crate) deadline: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilesRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) cwd: std::path::PathBuf,
}

impl PaneClickState {
    fn is_double_click_for(self, next: Self) -> bool {
        self.pane_id == next.pane_id
            && next.at.duration_since(self.at) <= PANE_DOUBLE_CLICK_WINDOW
            && self.viewport_row.abs_diff(next.viewport_row) <= 1
            && self.col.abs_diff(next.col) <= 1
    }
}

pub struct App {
    pub state: AppState,
    /// Attached client whose overlay is currently swapped into `state`.
    /// Deferred worktree operations copy this id into their completion event.
    pub(crate) active_overlay_client_id: Option<u64>,
    /// A focus completion selected the current client's pane. The headless
    /// server drains this after the mutation so external API focus can be
    /// projected into each attached client's presentation.
    pub(crate) pending_client_pane_focus: bool,
    pub(crate) pane_graphics: pane_graphics::Runtime,
    pub(crate) pane_graphics_files: Arc<crate::pane_graphics_files::FileStore>,
    pub(crate) direct_graphics_available: bool,
    pub(crate) pixel_mouse_available: bool,
    pub(crate) status_metric_refresh: crate::platform::status_metrics::StatusMetricRefresh,
    pub(crate) status_metric_sampler:
        Arc<std::sync::Mutex<crate::platform::status_metrics::StatusMetricSampler>>,
    pub(crate) status_metric_refresh_enabled: bool,
    pub(crate) status_metrics_visible: bool,
    pub(crate) provider_usage_refreshed_at: Option<Instant>,
    pub(crate) provider_usage_in_flight: bool,
    pub(crate) usage_scan_generation: u64,
    pub(crate) usage_scan_in_flight: Option<u64>,
    pub(crate) connectivity_probed_at: Option<Instant>,
    pub(crate) connectivity_probe_in_flight: bool,
    pub(crate) terminal_runtimes: crate::terminal::TerminalRuntimeRegistry,
    pub(crate) status_log: Option<crate::status_log::StatusLog>,
    /// Server-owned remote focus operations. This state is touched only by
    /// API requests and transport events, never by render or pane loops.
    pub(crate) remote_focus_operations: remote_focus::RemoteFocusOperations,
    pub(crate) planning_lock_key_burst: std::collections::VecDeque<Instant>,
    pub(crate) window_cycle_snapshot: Option<crate::app::input::WindowCycleSnapshot>,
    pub(crate) fleet_attach_agents:
        std::collections::HashMap<crate::layout::PaneId, crate::api::schema::AgentRef>,
    pub(crate) remote_focus_transport: Box<dyn remote_focus::RemoteFocusTransport>,
    pub(crate) fleet_poller_config: crate::fleet::FleetPollerHandle,
    /// Server-owned group authority. Persistence is separate from client presentation state.
    pub(crate) group_runtime: crate::groups::Runtime,
    /// Local durable root for server-owned day items. Tests opt in with an
    /// isolated path so AppState construction remains filesystem-free.
    pub(crate) day_store_root: Option<std::path::PathBuf>,
    /// Advance-only authority history, independent from current fleet routes.
    pub(crate) authority_acceptance_ledger: crate::fleet::AuthorityAcceptanceLedger,
    pub(crate) authority_acceptance_ledger_path: Option<std::path::PathBuf>,
    /// An unreadable non-missing ledger quarantines remote catalogs.
    pub(crate) authority_acceptance_ledger_error: Option<String>,
    /// Serial ledger writer. Only its completion events publish new catalogs.
    pub(crate) authority_acceptance_ledger_writer: crate::fleet::AuthorityAcceptanceLedgerWriter,
    pub(crate) authority_acceptance_ledger_write_in_flight: bool,
    pub(crate) pending_authority_acceptance_ledger: Option<crate::fleet::AuthorityAcceptanceLedger>,
    /// Advance-only history admitted while another ledger write is in flight.
    pub(crate) queued_authority_acceptance_ledger: Option<crate::fleet::AuthorityAcceptanceLedger>,
    /// Latest raw poll to present after every admitted advance is durable.
    pub(crate) queued_fleet_snapshot: Option<crate::fleet::Snapshot>,
    /// Serial remote mutation transport, kept off the app event loop.
    pub(crate) authority_mutation_router: crate::fleet::AuthorityMutationRouter,
    /// Owner-only pane memberships maintained at lifecycle boundaries so
    /// fleet snapshot requests never walk the pane tree.
    pub(crate) group_membership_projection:
        std::collections::BTreeMap<String, crate::groups::OwnedPaneMembership>,
    #[cfg(test)]
    pub(crate) group_session_paths_override: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// Runtime-only markers for shell panes launched by git and user actions.
    pub(crate) git_action_panes: HashMap<crate::layout::PaneId, git_actions::GitActionPaneState>,
    pub event_tx: mpsc::Sender<AppEvent>,
    pub(crate) event_rx: mpsc::Receiver<AppEvent>,
    pub(crate) api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
    pub(crate) event_hub: crate::api::EventHub,
    pub(crate) api_pane_state_updates: Option<Vec<crate::app::actions::PaneStateUpdate>>,
    pub(crate) loop_history_reader: Option<crate::loop_runs::ReceiptReader>,
    pub(crate) loop_receipt_watch_health: Arc<crate::loop_runs::ReceiptWatchHealth>,
    pub(crate) _loop_receipt_watcher: Option<notify::RecommendedWatcher>,
    /// Held only to keep the scratchpad watcher alive; replaced when focus moves
    /// to a different repository.
    pub(crate) scratchpad_watcher: Option<notify::RecommendedWatcher>,
    pub(crate) scratchpad_watched_path: Option<std::path::PathBuf>,
    /// Held only to keep the notepad directory watcher alive.
    pub(crate) notepad_watcher: Option<notify::RecommendedWatcher>,
    pub(crate) notepad_watched_dir: Option<std::path::PathBuf>,
    /// Note names the operator wants offered first, from `[notepad] files`.
    pub(crate) notepad_preferred_files: Vec<String>,
    /// The `[notepad] height` last applied from config. A reload that did not
    /// change the key leaves a dragged panel height alone.
    pub(crate) applied_notepad_config_height: Option<u16>,
    pub(crate) notepad_git_sync: bool,
    pub(crate) notepad_git_sync_interval: std::time::Duration,
    pub(crate) notepad_next_git_pull: Option<Instant>,
    pub(crate) goals_observation: Option<crate::goals::GoalsObservation>,
    pub(crate) goals_focused_cwd: Option<std::path::PathBuf>,
    pub(crate) goals_refresh_in_flight: bool,
    pub(crate) goals_refresh_generation: u64,
    pub(crate) next_goals_refresh: Instant,
    /// `[pomodoro] log_file`; empty disables the break log.
    pub(crate) pomodoro_log_file: String,
    pub(crate) loop_receipt_fallback_deadline: Option<Instant>,
    pub(crate) loop_receipt_watch_degraded: bool,
    pub(crate) last_focus: Option<(usize, crate::layout::PaneId)>,
    pub(crate) status_context_focus: Option<(String, usize, crate::layout::PaneId)>,
    pub(crate) no_session: bool,
    pub(crate) input_rx: Option<mpsc::Receiver<crate::raw_input::RawInputEvent>>,
    pub(crate) last_terminal_size: Option<(u16, u16)>,
    pub(crate) config_diagnostic_deadline: Option<Instant>,
    pub(crate) toast_deadline: Option<Instant>,
    pub(crate) copy_feedback_deadline: Option<Instant>,
    pub(crate) last_api_notification_at: Option<Instant>,
    pub(crate) missing_terminal_notification_backend_warned: std::cell::Cell<bool>,
    pub(crate) last_git_remote_status_refresh: Instant,
    pub(crate) last_git_repo_discovery_refresh: Instant,
    pub(crate) git_refresh_in_flight: Option<GitRefreshInFlight>,
    pub(crate) last_git_refresh_generation: u64,
    pub(crate) git_refresh_due_after_in_flight: bool,
    pub(crate) git_identity_refresh_requested: bool,
    pub(crate) git_status_cache: HashMap<std::path::PathBuf, crate::workspace::GitStatusCacheEntry>,
    pub(crate) diff_refresh_in_flight: Option<(u64, diff::DiffRefreshTarget)>,
    pub(crate) last_diff_refresh_generation: u64,
    pub(crate) files_refresh_in_flight: Option<FilesRefreshInFlight>,
    pub(crate) last_files_refresh_generation: u64,
    pub(crate) dock_files_refresh_demand: bool,
    pub(crate) home_ref_refreshes_in_flight: HashSet<std::path::PathBuf>,
    pub(crate) git_work_context_refresh_in_flight:
        Option<work_context_git::GitWorkContextRefreshInFlight>,
    pub(crate) git_work_context_refresh_due_after_in_flight: bool,
    pub(crate) git_work_context_rotation: usize,
    pub(crate) last_git_work_context_refresh_generation: u64,
    pub(crate) last_applied_git_work_context_refresh_generation: u64,
    pub(crate) next_git_work_context_refresh: Instant,
    pub(crate) git_work_context_cache: HashMap<
        work_context_git::GitWorkContextCacheKey,
        work_context_git::GitWorkContextCacheEntry,
    >,
    pub(crate) git_work_context_inputs:
        HashMap<crate::layout::PaneId, work_context_git::GitWorkContextInput>,
    pub(crate) work_index_config: crate::config::WorkIndexConfig,
    pub(crate) missive_config: crate::config::MissiveConfig,
    pub(crate) work_index_refresh_in_flight: Option<crate::work_index::WorkIndexRefreshInFlight>,
    pub(crate) last_work_index_refresh_generation: u64,
    pub(crate) last_applied_work_index_refresh_generation: u64,
    pub(crate) next_work_index_refresh: Instant,
    /// A refresh a person asked for, rather than one the interval came round to.
    /// A detached server stops polling on its own after a while; it must not also
    /// swallow the answer someone is waiting on.
    pub(crate) work_index_refresh_requested: bool,
    pub(crate) work_index_context_fingerprint: crate::work_index::WorkIndexContextFingerprint,
    pub(crate) work_index_cache_bypass: crate::work_index::WorkIndexCacheBypass,
    pub(crate) work_index_snapshot: Option<crate::work_index::Snapshot>,
    pub(crate) work_index_session: crate::work_index::WorkIndexSession,
    pub(crate) work_item_detail_refresh_in_flight:
        Option<crate::work_index::WorkItemDetailRefreshInFlight>,
    pub(crate) last_work_item_detail_refresh_generation: u64,
    pub(crate) last_applied_work_item_detail_refresh_generation: u64,
    pub(crate) foreground_process_refresh_in_flight:
        Option<foreground_process::ForegroundProcessRefreshInFlight>,
    pub(crate) last_foreground_process_refresh_generation: u64,
    pub(crate) last_applied_foreground_process_refresh_generation: u64,
    pub(crate) next_foreground_process_refresh: Instant,
    pub(crate) claude_subagent_refresh_in_flight: Option<claude_subagents::RefreshInFlight>,
    pub(crate) last_claude_subagent_refresh_generation: u64,
    pub(crate) last_applied_claude_subagent_refresh_generation: u64,
    pub(crate) next_claude_subagent_refresh: Instant,
    pub(crate) next_claude_subagent_target_generation: u64,
    pub(crate) claude_subagent_refresh_rotation: usize,
    pub(crate) claude_subagent_trackers:
        HashMap<crate::terminal::TerminalId, claude_subagents::TranscriptTracker>,
    pub(crate) last_status_observed: Option<Instant>,
    pub(crate) last_status_states: HashMap<crate::terminal::TerminalId, crate::detect::AgentState>,
    pub(crate) status_log_sink: status_log::StatusLogSink,
    #[cfg(test)]
    pub(crate) git_program_override: Option<std::path::PathBuf>,
    #[cfg(test)]
    pub(crate) work_index_gh_program_override: Option<std::path::PathBuf>,
    #[cfg(test)]
    pub(crate) work_index_provider_cache_root_override: Option<std::path::PathBuf>,
    #[cfg(test)]
    pub(crate) work_index_linearis_program_override: Option<std::path::PathBuf>,
    #[cfg(test)]
    pub(crate) work_index_curl_program_override: Option<std::path::PathBuf>,
    pub(crate) pending_api_worktree_creates: HashMap<std::path::PathBuf, u64>,
    pub(crate) pending_api_worktree_removes: HashMap<String, u64>,
    pub(crate) pending_api_worktree_remove_paths: HashMap<std::path::PathBuf, u64>,
    pub(crate) next_api_worktree_operation_id: u64,
    pub(crate) last_sidebar_divider_click: Option<Instant>,
    pub(crate) last_dock_divider_click: Option<Instant>,
    pub(crate) last_pane_click: Option<PaneClickState>,
    pub(crate) pending_url_click_sources: HashSet<InputSourceId>,
    /// Swallows the rest of a click after its press dismisses the marker.
    pub(crate) pending_config_diagnostic_click_sources: HashSet<InputSourceId>,
    pub(crate) pending_pomodoro_send_off_mouse_releases:
        HashMap<InputSourceId, crossterm::event::MouseButton>,
    pub(crate) contract_false_positive_burst_panes: HashSet<crate::layout::PaneId>,
    pub(crate) contract_false_positive_log_path_override: Option<std::path::PathBuf>,
    pub(crate) next_resize_poll: Instant,
    pub(crate) next_auto_update_check: Option<Instant>,
    pub(crate) next_agent_manifest_update_check: Option<Instant>,
    pub(crate) update_version_check_enabled: bool,
    pub(crate) update_manifest_check_enabled: bool,
    pub(crate) loaded_host_cursor: crate::config::HostCursorModeConfig,
    pub(crate) agent_metadata_deadline: Option<Instant>,
    pub(crate) agent_activity_refresh_deadline: Option<Instant>,
    pub(crate) pending_agent_resume_deadline: Option<Instant>,
    pub(crate) pending_agent_resume_retries: std::collections::HashMap<
        crate::terminal::TerminalId,
        agent_resume::PendingAgentResumeRetry,
    >,
    /// Agents that were just resumed into a native session and are waiting to
    /// be nudged back into their work, keyed by the terminal they run in.
    pub(crate) pending_resume_nudges:
        std::collections::HashMap<crate::terminal::TerminalId, agent_resume::ResumeNudge>,
    /// Runtime-only nudge budgets for currently stale agent declarations.
    pub(crate) stall_nudge_episodes:
        std::collections::HashMap<crate::terminal::TerminalId, auto_nudge::StallNudgeEpisode>,
    pub(crate) pending_stall_nudge_submissions: std::collections::HashMap<
        crate::terminal::TerminalId,
        auto_nudge::PendingStallNudgeSubmission,
    >,
    pub(crate) selection_autoscroll_deadline: Option<Instant>,
    pub(crate) selection_highlight_clear_deadline: Option<Instant>,
    pub(crate) session_save_deadline: Option<Instant>,
    pub(crate) session_save_scheduled_revision: Option<u64>,
    pub(crate) session_save_thread: Option<std::thread::JoinHandle<session::SessionSaveResult>>,
    pub(crate) session_save_failures: u32,
    pub(crate) session_save_retry_deadline: Option<Instant>,
    pub(crate) session_dirty_since: Option<Instant>,
    pub(crate) session_save_stall_warning_at: Option<Instant>,
    session_writer: Arc<std::sync::Mutex<crate::persist::SessionWriter>>,
    pane_exit_checkpoint_requests: HashSet<crate::layout::PaneId>,
    pane_exit_checkpoint_pending: bool,
    pub(crate) detached_custom_command_children: Vec<std::process::Child>,
    pub(crate) detached_process_children: Vec<std::process::Child>,
    tab_bar_status_generation: u64,
    tab_bar_hostname_segment: Option<usize>,
    tab_bar_datetimes: Vec<tab_bar_status::TabBarDatetimeRuntime>,
    tab_bar_commands: Vec<tab_bar_status::TabBarCommandRuntime>,
    next_tab_bar_datetime_refresh: Option<Instant>,
    /// Parsed `ui.window_title` plus the hostname resolved when it was applied.
    pub(crate) window_title_template: Option<(crate::config::WindowTitleTemplate, String)>,
    pub(crate) persist_pane_history: bool,
    /// Last render-loop attempt, including a throttled hidden-only PTY skip.
    pub(crate) last_render_at: Option<Instant>,
    /// Pomodoro surfaces shown by the last frame committed to the local client.
    pub(crate) local_pomodoro_presentation: Option<crate::ui::pomodoro::InputPresentation>,
    pub(crate) pending_first_frame_pane: Option<crate::layout::PaneId>,
    /// Last attempt that could update a connected presentation surface.
    pub(crate) last_presentation_at: Option<Instant>,
    pub(crate) input_leases: input::InputLeaseTable,
    pub render_notify: Arc<Notify>,
    pub(crate) render_dirty: Arc<crate::render_signal::RenderSignal>,
    pub(crate) full_redraw_pending: bool,
    pub(crate) overlay_panes: HashMap<crate::layout::PaneId, OverlayPaneState>,
    pub(crate) local_terminal_notifications: bool,
    /// Whether this process applies `AppEvent::PrefixInputSource` to the host input source.
    /// The headless server sets this to false: the switch belongs to the foreground client,
    /// even when an App-internal drain consumes the event before the forwarding drain.
    pub(crate) local_input_source_switch: bool,
    pub(crate) config_reloaded_from_disk: bool,
    prefix_input_source: Box<dyn crate::platform::PrefixInputSource>,
}

pub(crate) const APP_EVENT_CHANNEL_CAPACITY: usize = 256;
pub(crate) const APP_EVENT_DRAIN_LIMIT: usize = 64;

pub(crate) enum LoopEvent {
    Timer,
    Internal(AppEvent),
    Api(Box<crate::api::ApiRequestMessage>),
    RawInput(crate::raw_input::RawInputEvent),
    InputClosed,
    RenderRequested,
}

struct SyncOutputGuard;

impl SyncOutputGuard {
    fn begin() -> io::Result<Self> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"\x1b[?2026h")?;
        stdout.flush()?;
        Ok(Self)
    }
}

impl Drop for SyncOutputGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(b"\x1b[?2026l");
        let _ = stdout.flush();
    }
}

async fn recv_raw_input_or_pending(
    input_rx: Option<&mut mpsc::Receiver<crate::raw_input::RawInputEvent>>,
) -> Option<crate::raw_input::RawInputEvent> {
    match input_rx {
        Some(rx) => rx.recv().await,
        None => pending().await,
    }
}

async fn sleep_until_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => pending().await,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalInputTarget {
    terminal_id: crate::terminal::TerminalId,
}

impl TerminalInputTarget {
    pub(crate) fn new(terminal_id: crate::terminal::TerminalId) -> Self {
        Self { terminal_id }
    }

    #[cfg(unix)]
    pub(crate) fn terminal_id(&self) -> &crate::terminal::TerminalId {
        &self.terminal_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalInputContext {
    Pane,
    Popup(crate::terminal::TerminalId),
}

pub(crate) type InputSourceId = u64;
const LOCAL_INPUT_SOURCE: InputSourceId = 0;

fn auto_updates_enabled(no_session: bool) -> bool {
    !no_session && !cfg!(debug_assertions)
}

fn background_update_check_enabled(no_session: bool, check_enabled: bool) -> bool {
    auto_updates_enabled(no_session) && check_enabled
}

fn window_cycle_mode_after_restore(
    config_mode: crate::config::WindowCycleModeConfig,
    _snapshot_mode: Option<crate::config::WindowCycleModeConfig>,
) -> crate::config::WindowCycleModeConfig {
    config_mode
}

fn load_plugin_registry(no_session: bool) -> crate::app::state::InstalledPluginRegistry {
    if no_session {
        return std::collections::HashMap::new();
    }
    let entries = crate::persist::plugin_registry::load();
    let entries = crate::persist::plugin_registry::reload_manifests(entries, |path, enabled| {
        crate::app::api::plugins::load_plugin_manifest(path, enabled).map_err(|(_, msg)| msg)
    });
    entries
        .into_iter()
        .map(|plugin| (plugin.plugin_id.clone(), plugin))
        .collect()
}

fn agent_panel_sort_from_config(
    sort: crate::config::AgentPanelSortConfig,
) -> state::AgentPanelSort {
    match sort {
        crate::config::AgentPanelSortConfig::Spaces => state::AgentPanelSort::Spaces,
        crate::config::AgentPanelSortConfig::Priority => state::AgentPanelSort::Priority,
    }
}

fn dock_surfaces_from_config(panel: &crate::config::PanelConfig) -> Vec<state::DockSurface> {
    let mut surfaces = Vec::new();
    for configured in &panel.default_surfaces {
        let surface = match configured {
            crate::config::PanelSurfaceConfig::Home => state::DockSurface::Home,
            crate::config::PanelSurfaceConfig::Terminal => state::DockSurface::Terminal,
            crate::config::PanelSurfaceConfig::Files => state::DockSurface::Files,
            crate::config::PanelSurfaceConfig::Diff => state::DockSurface::Diff,
            crate::config::PanelSurfaceConfig::PullRequest => state::DockSurface::Pr,
            crate::config::PanelSurfaceConfig::Linear => state::DockSurface::Linear,
            crate::config::PanelSurfaceConfig::Missive => state::DockSurface::Missive,
            crate::config::PanelSurfaceConfig::Agents => state::DockSurface::Agents,
            crate::config::PanelSurfaceConfig::Hosts => state::DockSurface::Hosts,
            crate::config::PanelSurfaceConfig::Editor => state::DockSurface::Editor,
            crate::config::PanelSurfaceConfig::Shortcuts => state::DockSurface::Shortcuts,
            crate::config::PanelSurfaceConfig::Context => state::DockSurface::Context,
            crate::config::PanelSurfaceConfig::Scratchpad => state::DockSurface::Scratchpad,
        };
        if !surfaces.contains(&surface) {
            surfaces.push(surface);
        }
    }
    surfaces
}

/// Parse the configured agent name list into a deduplicated set of `Agent`
/// values. Unknown agent names are silently dropped so a typo cannot disable
/// other valid entries.
fn parse_cjk_ime_agents(names: &[String]) -> Vec<crate::detect::Agent> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some(agent) = crate::detect::parse_agent_label(name) {
            if !out.contains(&agent) {
                out.push(agent);
            }
        }
    }
    out
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn sibling_theme_names(name: &str) -> (String, String) {
    match normalize_theme_name(name).as_str() {
        "catppuccin" | "catppuccin-mocha" | "catppuccin-latte" | "latte" | "light" => {
            ("catppuccin".to_string(), "catppuccin-latte".to_string())
        }
        "tokyo-night" | "tokyonight" | "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => {
            ("tokyo-night".to_string(), "tokyo-night-day".to_string())
        }
        "gruvbox" | "gruvbox-dark" | "gruvbox-light" => {
            ("gruvbox".to_string(), "gruvbox-light".to_string())
        }
        "one-dark" | "onedark" | "one-light" | "onelight" => {
            ("one-dark".to_string(), "one-light".to_string())
        }
        "github-dark-high-contrast"
        | "github-dark-hc"
        | "github-light-high-contrast"
        | "github-light-hc" => (
            "github-dark-high-contrast".to_string(),
            "github-light-high-contrast".to_string(),
        ),
        "solarized" | "solarized-dark" | "solarized-light" => {
            ("solarized".to_string(), "solarized-light".to_string())
        }
        "kanagawa" | "kanagawa-lotus" | "lotus" => {
            ("kanagawa".to_string(), "kanagawa-lotus".to_string())
        }
        "rose-pine" | "rosepine" | "rose-pine-dawn" | "rosepine-dawn" | "dawn" => {
            ("rose-pine".to_string(), "rose-pine-dawn".to_string())
        }
        _ => (name.to_string(), name.to_string()),
    }
}

fn theme_runtime_config(
    config: &crate::config::Config,
    use_legacy_ui_accent: bool,
) -> state::ThemeRuntimeConfig {
    let manual_name = config
        .theme
        .name
        .clone()
        .unwrap_or_else(|| "catppuccin".to_string());
    let (default_dark, default_light) = sibling_theme_names(&manual_name);
    state::ThemeRuntimeConfig {
        manual_name,
        dark_name: config.theme.dark_name.clone().unwrap_or(default_dark),
        light_name: config.theme.light_name.clone().unwrap_or(default_light),
        auto_switch: config.theme.auto_switch,
        host_appearance: config.theme.host_appearance,
        runtime_host_appearance: None,
        custom: config.theme.custom.clone(),
        legacy_accent: (use_legacy_ui_accent
            && config.ui.accent != "cyan"
            && config
                .theme
                .custom
                .as_ref()
                .and_then(|c| c.accent.as_ref())
                .is_none())
        .then(|| config.ui.accent.clone()),
    }
}

fn resolve_palette_for_theme_name(
    name: &str,
    fallback_name: &str,
    runtime: &state::ThemeRuntimeConfig,
    mode_custom: Option<&crate::config::ModeThemeColors>,
) -> state::Palette {
    let mut palette = state::Palette::from_name(name).unwrap_or_else(|| {
        tracing::warn!(
            theme = name,
            fallback = fallback_name,
            "unknown theme, falling back"
        );
        state::Palette::from_name(fallback_name).unwrap_or_else(state::Palette::catppuccin)
    });

    if let Some(custom) = &runtime.custom {
        palette = palette.with_overrides(custom);
    }
    if let Some(accent) = &runtime.legacy_accent {
        palette.accent = crate::config::parse_color(accent);
    }
    if let Some(custom) = mode_custom {
        palette = palette.with_mode_overrides(custom);
    }

    palette
}

fn resolve_effective_theme(
    runtime: &state::ThemeRuntimeConfig,
    appearance: Option<crate::terminal_theme::HostAppearance>,
) -> (state::Palette, String) {
    let appearance = runtime.host_appearance.appearance().or(appearance);
    let (name, fallback, mode_custom) = if runtime.auto_switch {
        match appearance {
            Some(crate::terminal_theme::HostAppearance::Dark) => (
                &runtime.dark_name,
                "catppuccin",
                runtime
                    .custom
                    .as_ref()
                    .and_then(|custom| custom.dark.as_ref()),
            ),
            Some(crate::terminal_theme::HostAppearance::Light) => (
                &runtime.light_name,
                "catppuccin-latte",
                runtime
                    .custom
                    .as_ref()
                    .and_then(|custom| custom.light.as_ref()),
            ),
            // No OSC 11 answer has arrived yet. Guessing an appearance is how
            // auto_switch earned its reputation: a wrong guess paints one
            // appearance's foregrounds over the other's background, and the
            // result is unreadable rather than merely off-brand. Hold the
            // configured palette until the attached client reports for real.
            None => (&runtime.manual_name, "catppuccin", None),
        }
    } else {
        (&runtime.manual_name, "catppuccin", None)
    };
    (
        resolve_palette_for_theme_name(name, fallback, runtime, mode_custom),
        name.clone(),
    )
}

impl App {
    pub fn new(
        config: &Config,
        no_session: bool,
        config_diagnostic: Option<String>,
        api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
        event_hub: crate::api::EventHub,
    ) -> Self {
        let (prefix_code, prefix_mods) = config.prefix_key();
        crate::kitty_graphics::set_enabled(config.experimental.kitty_graphics);
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>(APP_EVENT_CHANNEL_CAPACITY);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(crate::render_signal::RenderSignal::new());
        let receipt_path = crate::loop_runs::default_receipt_path();
        let mut loop_history_reader = receipt_path
            .clone()
            .map(crate::loop_runs::ReceiptReader::new);
        let initial_loop_history = if let Some(reader) = loop_history_reader.as_mut() {
            reader.refresh();
            reader.history().clone()
        } else {
            crate::loop_runs::RunHistory::default()
        };
        let loop_receipt_watch = if cfg!(test) {
            crate::loop_runs::disabled_receipt_watch()
        } else {
            crate::loop_runs::watch_receipts(receipt_path, event_tx.clone())
        };
        let loop_receipt_watch_health = loop_receipt_watch.health.clone();
        let loop_receipt_fallback_deadline = loop_receipt_watch
            .fallback_enabled
            .then_some(Instant::now());
        let loop_receipt_watcher = loop_receipt_watch.watcher;

        // Try to restore previous session
        let mut restored_terminals = std::collections::HashMap::new();
        let mut restored_terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let restored_snapshot = (!no_session).then(crate::persist::load).flatten();
        let session_writer = Arc::new(std::sync::Mutex::new(crate::persist::SessionWriter::new(
            !no_session && restored_snapshot.is_none(),
        )));
        let (
            workspaces,
            active,
            selected,
            sidebar_width,
            sidebar_width_source,
            sidebar_section_split,
            collapsed_space_keys,
            prio_panel_collapsed,
            window_cycle_mode,
            skip_collapsed_cycle,
        ) = if no_session {
            (
                Vec::new(),
                None,
                0,
                config.ui.sidebar_width,
                state::SidebarWidthSource::ConfigDefault,
                0.5_f32,
                std::collections::HashSet::new(),
                false,
                config.ui.window_cycle_mode,
                config.ui.skip_collapsed_cycle,
            )
        } else if let Some(snap) = restored_snapshot {
            let history = config
                .experimental
                .pane_history
                .then(crate::persist::load_history)
                .flatten();
            let (ws, terminals, terminal_runtimes) = crate::persist::restore(
                &snap,
                history.as_ref(),
                24,
                80,
                config.advanced.scrollback_limit_bytes,
                &config.terminal.default_shell,
                config.terminal.shell_mode,
                config.session.resume_agents_on_restore,
                event_tx.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            );
            restored_terminals = terminals;
            restored_terminal_runtimes = terminal_runtimes.into();
            if ws.is_empty() {
                crate::logging::session_restored(0, "empty");
                (
                    Vec::new(),
                    None,
                    0,
                    snap.sidebar_width.unwrap_or(config.ui.sidebar_width),
                    if snap.sidebar_width.is_some() {
                        state::SidebarWidthSource::Persisted
                    } else {
                        state::SidebarWidthSource::ConfigDefault
                    },
                    snap.sidebar_section_split.unwrap_or(0.5),
                    snap.collapsed_space_keys,
                    snap.prio_panel_collapsed,
                    window_cycle_mode_after_restore(
                        config.ui.window_cycle_mode,
                        snap.window_cycle_mode,
                    ),
                    snap.skip_collapsed_cycle
                        .unwrap_or(config.ui.skip_collapsed_cycle),
                )
            } else {
                crate::logging::session_restored(ws.len(), "ok");
                let active = snap.active.filter(|&i| i < ws.len());
                let selected = snap.selected.min(ws.len().saturating_sub(1));
                (
                    ws,
                    active,
                    selected,
                    snap.sidebar_width.unwrap_or(config.ui.sidebar_width),
                    if snap.sidebar_width.is_some() {
                        state::SidebarWidthSource::Persisted
                    } else {
                        state::SidebarWidthSource::ConfigDefault
                    },
                    snap.sidebar_section_split.unwrap_or(0.5),
                    snap.collapsed_space_keys,
                    snap.prio_panel_collapsed,
                    window_cycle_mode_after_restore(
                        config.ui.window_cycle_mode,
                        snap.window_cycle_mode,
                    ),
                    snap.skip_collapsed_cycle
                        .unwrap_or(config.ui.skip_collapsed_cycle),
                )
            }
        } else {
            (
                Vec::new(),
                None,
                0,
                config.ui.sidebar_width,
                state::SidebarWidthSource::ConfigDefault,
                0.5_f32,
                std::collections::HashSet::new(),
                false,
                config.ui.window_cycle_mode,
                config.ui.skip_collapsed_cycle,
            )
        };

        let agent_panel_sort = agent_panel_sort_from_config(config.ui.agent_panel_sort);
        let agent_host_name = config.remote.fleet.resolved_self_name();
        let chrome_host_label = config
            .remote
            .fleet
            .chrome_host_label_for_resolved_name(&agent_host_name);
        let local_agent_panel_identities =
            crate::ui::local_agent_panel_identities(&workspaces, &agent_host_name);
        let dock_default_surfaces = dock_surfaces_from_config(&config.panel);
        let dock_tab = dock_default_surfaces.first().copied();

        // Validate sidebar bounds before they reach any `u16::clamp(min, max)`
        // call: `clamp` panics when `min > max`. On bad config, fall back to
        // the built-in defaults rather than crashing on the first render.
        let (sidebar_min_width, sidebar_max_width) = crate::config::validated_sidebar_bounds(
            config.ui.sidebar_min_width,
            config.ui.sidebar_max_width,
        )
        .unwrap_or_else(|| {
            tracing::warn!(
                min = config.ui.sidebar_min_width,
                max = config.ui.sidebar_max_width,
                "ui.sidebar_min_width is greater than sidebar_max_width; falling back to default bounds (18, 36)"
            );
            (18, 36)
        });

        let worktree_directory =
            crate::worktree::expand_tilde_absolute_path(&config.worktrees.directory);

        info!(
            pane_scrollback_limit_bytes = config.advanced.scrollback_limit_bytes,
            "using pane scrollback configuration"
        );

        let latest_release_notes = crate::release_notes::load_latest();
        let update_available = latest_release_notes
            .as_ref()
            .filter(|notes| notes.preview)
            .map(|notes| notes.version.clone());
        let latest_release_notes_available = latest_release_notes.is_some();
        let update_install_command = crate::update::update_install_command().to_string();
        let startup_product_announcement =
            crate::product_announcements::load_unseen_for_current_version();
        // Cold-start snapshot comes from the host state dir; unit tests must not
        // observe whatever work index the developer machine has persisted.
        #[cfg(not(test))]
        let work_index_snapshot =
            crate::work_index::load_snapshot(&crate::work_index::work_index_snapshot_path());
        #[cfg(test)]
        let work_index_snapshot: Option<crate::work_index::Snapshot> = None;
        let work_item_detail_cache =
            crate::work_index::WorkItemDetailCache::from_snapshot(work_index_snapshot.as_ref());

        let mode = if config.should_show_onboarding() {
            state::Mode::Onboarding
        } else if startup_product_announcement.is_some() {
            state::Mode::ProductAnnouncement
        } else if active.is_some() {
            state::Mode::Terminal
        } else {
            state::Mode::Navigate
        };

        #[cfg(not(test))]
        let agent_manifest_summaries = crate::detect::manifest::reload_manifests();
        // Nextest runs each unit test in a fresh process. Manifest-sensitive tests reload
        // explicitly; unrelated App tests should not recompile every bundled regex.
        #[cfg(test)]
        let agent_manifest_summaries = Vec::new();
        let theme_runtime = theme_runtime_config(config, true);
        let (theme_palette, theme_name) = resolve_effective_theme(&theme_runtime, None);
        #[cfg(not(test))]
        let sidebar_group_mode = crate::client::presentation::load_sidebar_group_mode();
        #[cfg(test)]
        let sidebar_group_mode = state::SidebarGroupMode::default();
        #[cfg(not(test))]
        let sidebar_work_filter = crate::client::presentation::load_sidebar_work_filter();
        #[cfg(test)]
        let sidebar_work_filter = state::SidebarWorkFilter::default();
        #[cfg(not(test))]
        let sidebar_group_sorts = crate::client::presentation::load_sidebar_group_sorts();
        #[cfg(test)]
        let sidebar_group_sorts = std::collections::HashMap::new();
        #[cfg(not(test))]
        let sidebar_group_collapsed = crate::client::presentation::load_sidebar_group_collapsed();
        #[cfg(test)]
        let sidebar_group_collapsed = std::collections::HashMap::new();

        #[cfg(not(test))]
        let sidebar_presentation = state::SidebarPresentationState {
            spawn_dock_client_id: Some(crate::platform::client_presentation_identity()),
            ..Default::default()
        };
        #[cfg(test)]
        let sidebar_presentation = state::SidebarPresentationState::default();

        let mut state = AppState {
            agent_picker: None,
            collapsed_sidebar_groups: crate::ui::initial_collapsed_sidebar_groups(
                &sidebar_group_collapsed,
            ),
            sidebar_group_mode,
            sidebar_focused: false,
            client_focus_intent: state::ClientFocusIntent::FollowShared,
            sidebar_group_menu_open: false,
            sidebar_group_menu_selected: sidebar_group_mode.view_index(),
            sidebar_work_filter,
            sidebar_filter_menu_open: false,
            sidebar_filter_menu_selected: 0,
            sidebar_search_active: false,
            sidebar_starred_only: false,
            sidebar_new_menu: None,
            sidebar_areas_menu_selected: None,
            window_cycle_menu_open: false,
            window_cycle_menu_selected: 0,
            sidebar_new_thread: None,
            sidebar_project_menu: None,
            sidebar_refresh_requested: false,
            sidebar_refreshing: false,
            sidebar_selected_work_group: None,
            sidebar_object_menu: None,
            sidebar_sort_menu: None,
            sidebar_subgroup_picker: None,
            sidebar_pod_picker: None,
            sidebar_group_sorts,
            sidebar_unassigned_expanded_views: std::collections::HashSet::new(),
            sidebar_selected_settled: None,
            settled_view: None,
            sidebar_snooze: None,
            sidebar_settled_menu_target: None,
            sidebar_settled_menu_selected: 0,
            sidebar_settled_menu_delete_armed: false,
            pending_pane_settlement_changes: Vec::new(),
            pending_status_transitions: Vec::new(),
            pending_pane_snooze_changes: Vec::new(),
            view_observed_at: Instant::now(),
            view_observed_unix_s: settled::unix_seconds(std::time::SystemTime::now()),
            loop_run_history: initial_loop_history,
            loop_registry: crate::loop_runs::LoopRegistry::default(),
            loop_run_history_detail: None,
            aloop_run_detail: None,
            symphony_snapshot: crate::symphony::Snapshot::default(),
            fleet_snapshot: crate::fleet::Snapshot::unpolled(&config.remote.fleet.hosts),
            local_group_snapshot: None,
            agent_host_name,
            chrome_host_label,
            day_board: if cfg!(test) {
                crate::day::DayBoard::default()
            } else {
                crate::day::load(&crate::day::default_root())
            },
            day_stale_after: Duration::from_secs(config.day_board.stale_after),
            board_view: None,
            board_return: None,
            local_agent_panel_identities,
            remote_agent_panel_entries: Vec::new(),
            remote_agent_device_groups: None,
            remote_device_activity: None,
            agent_run_device_groups: None,
            remote_loop_device_groups: None,
            agent_runs_active_count: None,
            aloop_projection: None,
            remote_focus_proxy_panes: std::collections::HashSet::new(),
            sidebar_selected_remote_agent: None,
            dock_symphony: None,
            symphony_detail: None,
            work_view: None,
            linear_default_layout: config.linear.default_layout.into(),
            hovered_control: None,
            hover_started_at: None,
            hover_tooltip_visible: false,
            usage_view: None,
            usage_snapshot: if cfg!(test) {
                None
            } else {
                crate::provider_usage::load_cached_usage()
            },
            usage_pricing: config.usage.clone(),
            request_usage_scan: false,
            inbox: None,
            home: None,
            spawn_dock: None,
            home_agent_choices: Vec::new(),
            home_catalog: if cfg!(test) {
                crate::app::home_catalog::HomeCatalog::fallback()
            } else {
                crate::app::home_catalog::cached_home_catalog_for_current_profile()
            },
            home_ref_cache: std::collections::HashMap::new(),
            request_home_ref_refresh: None,
            request_tool_probes: false,
            pending_human_drafts: std::collections::HashMap::new(),
            status_metrics: None,
            status_git_cwd: None,
            status_git_branch: None,
            status_git_ahead_behind: None,
            status_focused_cwd: None,
            status_focus_projection_initialized: false,
            git_root_for_cwd: std::collections::HashMap::new(),
            repo_editor_argv: repo_editor::resolve_repo_editor_argv(),
            forwarded_pane_input: None,
            status_bar_enabled: config.ui.status_bar.enabled,
            full_lifecycle_hook_authority_timeout: std::time::Duration::from_secs(
                config
                    .agent_detection
                    .full_lifecycle_hook_authority_timeout_seconds,
            ),
            hide_done_after: std::time::Duration::from_secs(
                config.session.hide_done_after_minutes.saturating_mul(60),
            ),
            reap_done_after: std::time::Duration::from_secs(
                config.session.reap_done_after_minutes.saturating_mul(60),
            ),
            reap_done_panes: config.session.reap_done_panes,
            settle_after: std::time::Duration::from_secs(
                config
                    .session
                    .settle_after_days
                    .saturating_mul(24 * 60 * 60),
            ),
            settle_finished_after: std::time::Duration::from_secs(
                config
                    .session
                    .settle_finished_after_minutes
                    .saturating_mul(60),
            ),
            settle_done_after: std::time::Duration::from_secs(
                config.session.settle_done_after_minutes.saturating_mul(60),
            ),
            settled_read_only_after: std::time::Duration::from_secs(
                config
                    .session
                    .settled_read_only_after_minutes
                    .saturating_mul(60),
            ),
            terminals: std::collections::HashMap::new(),
            agent_states: crate::agent_state::AgentStateStore::default(),
            direct_attach_resize_locks: std::collections::HashSet::new(),
            pane_id_aliases: std::collections::HashMap::new(),
            public_pane_id_aliases: std::collections::HashMap::new(),
            workspaces,
            active,
            previous_pane_focus: None,
            selected,
            client_overlay: state::ClientOverlay::None,
            server_interaction: state::ServerInteractionState::new(mode),
            should_quit: false,
            detach_exits: no_session,
            detach_requested: false,
            request_new_workspace: false,
            request_new_tab: false,
            request_pane_toggle: None,
            request_open_repo_editor: false,
            notepad_request: None,
            request_git_action: None,
            request_user_action: None,
            request_save_add_action: false,
            request_pr_command: None,
            request_pin_toggle: None,
            request_new_linked_worktree: None,
            request_open_existing_worktree: None,
            request_new_workspace_cwd: None,
            request_remove_linked_worktree: None,
            request_submit_worktree_create: false,
            request_submit_worktree_open: false,
            request_submit_worktree_remove: false,
            request_reload_config: false,
            request_client_config_reload: false,
            request_client_notification_config: None,
            dock_width_persistence_request: None,
            notepad_height_persistence_request: None,
            sidebar_group_mode_persistence_request: None,
            sidebar_group_sort_persistence_request: None,
            sidebar_group_collapsed_persistence_request: None,
            sidebar_view_scan_request: false,
            sidebar_work_filter_persistence_request: None,
            request_clipboard_write: None,
            creating_new_tab: false,
            requested_new_tab_name: None,
            pending_workspace_create_cwd: None,
            rename_pane_target: None,
            worktree_create: None,
            worktree_open: None,
            worktree_remove: None,
            worktree_directory,
            collapsed_space_keys,
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_caret: 0,
            name_input_replace_on_type: false,
            rename_tab_prefill: None,
            release_notes: None,
            product_announcement: startup_product_announcement.map(|announcement| {
                state::ProductAnnouncementState {
                    version: announcement.version,
                    id: announcement.id,
                    title: announcement.title,
                    body: announcement.body,
                    scroll: 0,
                    preview: announcement.preview,
                }
            }),
            keybind_help: state::KeybindHelpState::default(),
            navigator: state::NavigatorState::default(),
            command_palette: state::CommandPaletteState::default(),
            work_link_picker: None,
            add_action: None,
            copy_mode: None,
            sidebar_presentation,
            sidebar_projection_revision: 0,
            workspace_picker_forces_spaces_tree: false,
            workspace_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            mobile_switcher_scroll: 0,
            view: state::ViewState {
                layout: state::ViewLayout::Desktop,
                status_bar_rect: Rect::default(),
                sidebar_rect: Rect::default(),
                sidebar_footer_settings_hit_area: Rect::default(),
                sidebar_footer_ask_subtitles_hit_area: Rect::default(),
                sidebar_footer_work_hit_area: Rect::default(),
                sidebar_footer_usage_hit_area: Rect::default(),
                usage_hit_areas: Vec::new(),
                sidebar_footer_ticket_hit_area: Rect::default(),
                sidebar_footer_missive_hit_area: Rect::default(),
                notepad_rect: Rect::default(),
                notepad_usage_toggle_hit_area: Rect::default(),
                notepad_tab_hit_areas: Vec::new(),
                notepad_agent_rows: Vec::new(),
                notepad_agent_max_scroll: 0,
                notepad_usage_rows: Vec::new(),
                notepad_usage_hit_areas: Vec::new(),
                notepad_usage_max_scroll: 0,
                pomodoro_hit_area: Rect::default(),
                notification_hit_area: Rect::default(),
                window_cycle_mode_hit_area: Rect::default(),
                sidebar_areas_hit_area: Rect::default(),
                hyperspace_rect: Rect::default(),
                hyperspace_pause_hit_area: Rect::default(),
                sidebar_footer_refresh_hit_area: Rect::default(),
                sidebar_footer_board_hit_area: Rect::default(),
                sidebar_footer_planning_lock_hit_area: Rect::default(),
                workspace_card_areas: Vec::new(),
                agent_card_areas: Vec::new(),
                sidebar_hover_targets: Vec::new(),
                visible_agent_activity_instants: Vec::new(),
                visible_notepad_agent_ages: Vec::new(),
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                repo_editor_button_hit_area: Rect::default(),
                add_action_button_hit_area: Rect::default(),
                user_action_hit_areas: Vec::new(),
                add_action_close_hit_area: Rect::default(),
                add_action_field_hit_areas: Vec::new(),
                add_action_cancel_hit_area: Rect::default(),
                add_action_save_hit_area: Rect::default(),
                add_project_layout: crate::ui::add_project::AddProjectLayout::default(),
                git_menu_button_hit_area: Rect::default(),
                git_menu_popup_rect: Rect::default(),
                git_menu_first_visible: 0,
                git_menu_row_hit_areas: Vec::new(),
                pane_toggle_below_hit_area: Rect::default(),
                pane_toggle_right_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                work_context_link_rows: Vec::new(),
                mobile_header_rect: Rect::default(),
                mobile_menu_hit_area: Rect::default(),
                config_diagnostic_hit_area: Rect::default(),
                toast_hit_area: Rect::default(),
                home_row_hit_areas: Vec::new(),
                home_hit_areas: Vec::new(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
                dock_rect: Rect::default(),
                dock_handle_rect: Rect::default(),
                dock_divider_rect: Rect::default(),
                dock_tab_bar_rect: Rect::default(),
                dock_tab_hit_areas: Vec::new(),
                dock_tab_close_rect: Rect::default(),
                dock_plus_rect: Rect::default(),
                dock_maximize_rect: Rect::default(),
                dock_auto_open_rect: Rect::default(),
                dock_surface_card_hit_areas: Vec::new(),
                dock_surface_menu_layout: None,
                dock_home_section_hit_areas: Vec::new(),
                dock_home_tab_hit_areas: Vec::new(),
                dock_home_tab_keys: Vec::new(),
                dock_home_detail_tab_hit_areas: Vec::new(),
                dock_file_row_hit_areas: Vec::new(),
                dock_files_refresh_rect: Rect::default(),
                dock_files_sort_rect: Rect::default(),
                editor_preview_refresh_rect: Rect::default(),
                editor_preview_open_rect: Rect::default(),
                dock_agent_row_hit_areas: Vec::new(),
                dock_host_row_hit_areas: Vec::new(),
                dock_body_rect: Rect::default(),
                scratchpad_link_rows: Vec::new(),
                status_buttons: Vec::new(),
                status_work_links: Vec::new(),
                status_segments: Vec::new(),
                status_segment_hit_areas: Vec::new(),
                focused_remote_host: None,
            },
            drag: None,
            workspace_presses: HashMap::new(),
            tab_presses: HashMap::new(),
            agent_presses: HashMap::new(),
            remote_agent_presses: HashMap::new(),
            selection: None,
            selection_autoscroll: None,
            context_menu: None,
            git_menu: state::MenuListState::new(0),
            update_available,
            update_install_command,
            latest_release_notes_available,
            update_dismissed: false,
            config_diagnostic,
            toast: None,
            pending_agent_notifications: std::collections::HashMap::new(),
            copy_feedback: None,
            outer_terminal_focus: None,
            prefix_code,
            prefix_mods,
            headless_size: config.headless_size(),
            default_sidebar_width: config.ui.sidebar_width,
            sidebar_width,
            sidebar_min_width,
            sidebar_max_width,
            dock_width: crate::ui::DOCK_DEFAULT_WIDTH,
            dock_collapsed: true,
            dock_default_surfaces: dock_default_surfaces.clone(),
            dock_surface_override: false,
            dock_tab,
            dock_open_surfaces: dock_default_surfaces.clone(),
            dock_tab_bindings: vec![None; dock_default_surfaces.len()],
            dock_active_tab_index: (!dock_default_surfaces.is_empty()).then_some(0),
            dock_pane_tabs: std::collections::HashMap::new(),
            dock_followed_pane: None,
            dock_context_objects: Vec::new(),
            dock_suppressed_context: std::collections::HashSet::new(),
            dock_maximized: false,
            dock_surface_menu: None,
            dock_chooser_focused: false,
            dock_scroll: 0,
            dock_object_preview: None,
            dock_object_views: std::collections::HashMap::new(),
            dock_editor_focused: false,
            dock_diff_focused: false,
            dock_pr_focused: false,
            dock_pr_checkout_menu: None,
            dock_pr_action_menu: None,
            dock_diff_ignore_whitespace: config.ui.hide_whitespace_in_diff,
            dock_diff_selected: 0,
            dock_diff_collapsed: std::collections::HashSet::new(),
            dock_diff_request: None,
            dock_diff_active_key: None,
            dock_diff_cache: std::collections::HashMap::new(),
            dock_diff_resolved_requests: std::collections::HashMap::new(),
            dock_files_focused: false,
            dock_files_selection: None,
            dock_files_filter: String::new(),
            dock_files_collapsed: std::collections::HashSet::new(),
            dock_files_sort: crate::files::FileSort::Name,
            dock_files_search_active: false,
            dock_files_last_click: None,
            dock_agents_focused: false,
            dock_agents_selection: None,
            dock_hosts_focused: false,
            dock_hosts_selection: None,
            dock_linear_focused: false,
            dock_ticket_start_menu: None,
            dock_ticket_action_menu: None,
            dock_ticket_comment_draft: None,
            dock_file_cache: std::collections::HashMap::new(),
            dock_files_root: None,
            dock_files_cwd: None,
            dock_files_roots_by_cwd: std::collections::HashMap::new(),
            files_icons: config.files.icons,
            nerd_font: config.ui.nerd_font,
            space_icons: config.ui.icons.spaces.clone(),
            dock_home_selection: None,
            dock_home_ticket_selection: None,
            dock_home_poll_selection: None,
            dock_home_focus_unbound: false,
            dock_comment_draft: None,
            dock_pending_write: None,
            pr_action_confirmation: None,
            dock_write_notice: None,
            dock_home_section: state::DockHomeSection::Prs,
            dock_home_detail_tab: state::DockHomeDetailTab::Overview,
            dock_home_focused: false,
            dock_home_followed_pane: None,
            work_index_snapshot: work_index_snapshot.clone(),
            settings_missive_team: config.missive.team.clone(),
            settings_missive_token_present: std::env::var_os(&config.missive.token_env)
                .is_some_and(|value| !value.is_empty()),
            work_index_session: crate::work_index::WorkIndexSession::default(),
            work_item_detail_cache,
            work_item_detail_loading: std::collections::HashSet::new(),
            work_index_enabled: config.work_index.enabled,
            pr_merge_method: config.source_control.merge_method,
            branch_prefix: config.source_control.branch_prefix.clone(),
            commit_message_model: config.source_control.commit_message_model.clone(),
            commit_stage_all: config.source_control.commit_stage_all,
            work_index_linear_team_configured: config
                .work_index
                .linear_team
                .as_deref()
                .is_some_and(|team| !team.trim().is_empty()),
            dock_editor_sessions: std::collections::HashMap::new(),
            dock_editor_preview: None,
            dock_editor_errors: std::collections::HashMap::new(),
            dock_editor_requested_paths: std::collections::HashMap::new(),
            scratchpad: crate::scratchpad::ScratchpadDoc::default(),
            notepad: crate::notepad::NotepadState::from_config(&config.notepad),
            sidebar_note_names: crate::app::state::sidebar_panel_note_names(
                &config.notepad.files,
                &config.pomodoro.log_file,
            ),
            goals: crate::goals::GoalsPanelState::from_config(&config.goals_panel),
            pomodoro: crate::pomodoro::PomodoroState::from_config(&config.pomodoro, Instant::now()),
            planning_lock: {
                let mut lock = if cfg!(test) {
                    crate::planning_lock::PlanningLock::default()
                } else {
                    let path =
                        crate::config::config_dir().join(crate::planning_lock::CONFIG_FILE_NAME);
                    crate::planning_lock::PlanningLock::load(&path)
                };
                if let Some(tab_id) = config.planning_lock.discussion_tab_id.as_deref() {
                    let _ = lock.set_local_discussion_tab(
                        tab_id,
                        crate::app::settled::unix_seconds(std::time::SystemTime::now()),
                    );
                }
                lock
            },
            planning_lock_dialog: None,
            hyperspace: crate::hyperspace::HyperspaceState::new(
                config.ui.sidebar_animation,
                Instant::now(),
            ),
            mobile_width_threshold: config.ui.mobile_width_threshold,
            sidebar_width_source,
            sidebar_width_auto: false,
            sidebar_collapsed: config.ui.sidebar_start_collapsed,
            blocked_filter: false,
            window_cycle_mode,
            skip_collapsed_cycle,
            sidebar_collapsed_mode: config.ui.sidebar_collapsed_mode,
            sidebar_section_split,
            prio_panel_collapsed,
            agent_panel_scroll: 0,
            agent_panel_sort,
            status_indicators: config.ui.status_indicators,
            working_row_opacity_percent: config.ui.working_row_opacity_percent,
            agent_view_override: None,
            sidebar_agents: config.ui.sidebar.agents.clone(),
            sidebar_spaces: config.ui.sidebar.spaces.clone(),
            sidebar_areas: config.ui.sidebar.areas.clone(),
            sidebar_sections_layout: config.ui.sidebar.layout
                == crate::config::SidebarLayoutConfig::Sections,
            sidebar_show_ask_subtitles: config.ui.sidebar.show_ask_subtitles,
            sidebar_header_plain: config.ui.sidebar.header
                == crate::config::SidebarHeaderConfig::Plain,
            next_agent_state_change_seq: 0,
            mouse_capture: config.ui.mouse_capture,
            copy_on_select: config.ui.copy_on_select,
            right_click_passthrough_modifiers: config.ui.right_click_passthrough_modifiers(),
            right_click_passthrough: None,
            redraw_on_focus_gained: config.ui.redraw_on_focus_gained,
            mouse_scroll_lines: config.ui.mouse_scroll_lines(),
            confirm_close: config.ui.confirm_close,
            combine_repos_across_hosts: config.ui.combine_repos_across_hosts,
            new_thread_workspace: config.ui.new_thread_workspace,
            launch_profiles: crate::app::launch_profiles::resolve(&config.launch_profiles),
            next_home_machine: None,
            next_home_profile: None,
            projects: crate::app::projects::resolve(&config.projects),
            machines: crate::app::machines::resolve(&config.remote.fleet),
            add_project_start_dir: config.ui.add_project_start_dir.clone(),
            auto_settle_finished: config.session.auto_settle_finished,
            auto_settle_inactive: config.session.auto_settle_inactive,
            auto_settle_done: config.session.auto_settle_done,
            settle_stops_agent: config.session.settle_stops_agent,
            nudge_resumed_agents: config.session.nudge_resumed_agents,
            resume_nudge_message: config.session.resume_nudge_message.clone(),
            auto_nudge_stalled_agents: config.session.auto_nudge_stalled_agents,
            agent_stale_after: auto_nudge::nudge_after_duration(
                config.session.agent_stale_after_minutes,
            ),
            agent_subagent_stale_after: auto_nudge::nudge_after_duration(
                config.session.agent_subagent_stale_after_minutes,
            ),
            nudge_after: auto_nudge::nudge_after_duration(config.session.nudge_after_minutes),
            max_nudges: config.session.max_nudges,
            max_nudges_without_human: config.session.max_nudges_without_human,
            stall_nudge_message: config.session.stall_nudge_message.clone(),
            prompt_new_tab_name: config.ui.prompt_new_tab_name,
            prompt_new_workspace_name: config.ui.prompt_new_workspace_name,
            pane_borders: config.ui.pane_borders,
            pane_outer_borders: config.ui.pane_outer_borders,
            pane_scrollbars: config.ui.pane_scrollbars,
            show_pull_button: config.ui.show_pull_button,
            open_dock_on_work_link: config.ui.open_dock_on_work_link,
            show_pane_toggle_buttons: config.ui.show_pane_toggle_buttons,
            pane_gaps: config.ui.pane_gaps,
            show_agent_labels_on_pane_borders: config.ui.show_agent_labels_on_pane_borders,
            hide_tab_bar_when_single_tab: config.ui.hide_tab_bar_when_single_tab,
            status_bar_expanded: config.ui.status_bar_expanded,
            fleet_status: config.ui.fleet_status,
            status_now_unix: crate::provider_usage::now_unix(),
            provider_usage: crate::provider_usage::ProviderUsageSnapshot::default(),
            connectivity: crate::connectivity::Connectivity::default(),
            status_disk_visible: false,
            tab_bar_position: config.ui.tab_bar_position,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: String::new(),
            pane_history_persistence: config.experimental.pane_history,
            reveal_hidden_cursor_for_cjk_ime: config.experimental.reveal_hidden_cursor_for_cjk_ime,
            cjk_ime_agent_filter_configured: !config.experimental.cjk_ime_agents.is_empty(),
            cjk_ime_agents: parse_cjk_ime_agents(&config.experimental.cjk_ime_agents),
            cjk_ime_cursor_shape: config.experimental.cjk_ime_cursor_shape.to_decscusr(),
            switch_ascii_input_source_in_prefix: config
                .experimental
                .switch_ascii_input_source_in_prefix,
            kitty_graphics_enabled: config.experimental.kitty_graphics,
            default_shell: config.terminal.default_shell.clone(),
            shell_mode: config.terminal.shell_mode,
            new_terminal_cwd: config.terminal.new_cwd.clone(),
            pane_scrollback_limit_bytes: config.advanced.scrollback_limit_bytes,
            accent: crate::config::parse_color(&config.ui.accent),
            sound: config.ui.sound.clone(),
            local_sound_playback: true,
            toast_config: config.ui.toast.clone(),
            last_non_off_toast_delivery: match config.ui.toast.delivery {
                crate::config::ToastDelivery::Off => crate::config::ToastDelivery::Terminal,
                delivery => delivery,
            },
            keybinds: config.keybinds(),
            palette: theme_palette,
            theme_name,
            theme_runtime,
            host_terminal_appearance: None,
            host_terminal_appearance_explicit: false,
            theme_appearance_mismatch: None,
            settings: state::SettingsState {
                section: state::SettingsSection::Theme,
                list: state::SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
                archive_delete_armed: false,
                search: String::new(),
                search_active: false,
                keybind_capture: None,
            },
            tool_probes: crate::app::probes::ToolProbeState::Idle,
            integration_recommendations: crate::integration::integration_recommendations(),
            agent_manifest_summaries,
            agent_manifest_update_status: crate::detect::manifest_update::load_status(),
            integration_install_messages: Vec::new(),
            installed_plugins: load_plugin_registry(no_session),
            plugin_panes: std::collections::HashMap::new(),
            popup_pane: None,
            plugin_command_logs: Vec::new(),
            next_plugin_command_log_id: 1,
            plugin_commands_in_flight: 0,
            global_menu: state::MenuListState::new(0),
            host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
            host_cell_size: crate::kitty_graphics::HostCellSize::default(),
            host_mouse_pixels: None,
            session_dirty: false,
            session_dirty_revision: 0,
            session_epoch: format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            session_event_revision: 0,
            terminal_runtime_shutdowns: Vec::new(),
            confirm_close_workspace_id: None,
            confirm_close_remote_agent_ref: None,
            rename_target: None,
        };

        state.terminals = restored_terminals;

        // The notepad's dragged height persists in the host's presentation
        // file, next to the dock width.
        #[cfg(not(test))]
        if let Some(height) = crate::client::presentation::load_notepad_height() {
            state.notepad.height = height;
        }

        for ws_idx in 0..state.workspaces.len() {
            let cwd = state.workspaces[ws_idx]
                .resolved_identity_cwd_from(&state.terminals, &restored_terminal_runtimes);
            state.workspaces[ws_idx].cached_git_branch =
                cwd.as_deref().and_then(crate::workspace::git_branch);
        }

        // Background auto-update is disabled in monolithic no-session mode
        // and in debug/test builds so local development never mutates the
        // running binary out from under spawned test processes.
        let version_check_enabled =
            background_update_check_enabled(no_session, config.update.version_check);
        let manifest_check_enabled =
            background_update_check_enabled(no_session, config.update.manifest_check);
        if version_check_enabled {
            let update_tx = event_tx.clone();
            std::thread::spawn(move || crate::update::auto_update(update_tx));
        }
        if manifest_check_enabled {
            let manifest_update_tx = event_tx.clone();
            std::thread::spawn(move || {
                crate::detect::manifest_update::auto_update(manifest_update_tx)
            });
        }
        if !cfg!(test) {
            let catalog_tx = event_tx.clone();
            std::thread::spawn(move || {
                if let Some(catalog) = crate::app::home_catalog::refresh_claude_catalog() {
                    let _ = catalog_tx.blocking_send(AppEvent::HomeCatalogRefreshed { catalog });
                }
                if let Some(catalog) = crate::app::home_catalog::refresh_codex_catalog() {
                    let _ = catalog_tx.blocking_send(AppEvent::HomeCatalogRefreshed { catalog });
                }
            });
        }
        let fleet_poller_config =
            crate::fleet::start_poller(config.remote.fleet.clone(), event_tx.clone());
        let authority_acceptance_ledger_writer =
            crate::fleet::AuthorityAcceptanceLedgerWriter::new(event_tx.clone());
        crate::symphony::start_poller(fleet_poller_config.clone(), event_tx.clone());
        #[cfg(not(test))]
        let group_runtime = crate::groups::Runtime::load_default();
        #[cfg(test)]
        let group_runtime = crate::groups::Runtime::unavailable_for_tests();
        #[cfg(not(test))]
        let authority_acceptance_ledger_path =
            Some(crate::fleet::authority_acceptance_ledger_path());
        #[cfg(not(test))]
        let legacy_group_catalog_cache_path = Some(crate::fleet::legacy_group_catalog_cache_path());
        #[cfg(test)]
        let authority_acceptance_ledger_path = None;
        #[cfg(test)]
        let legacy_group_catalog_cache_path: Option<std::path::PathBuf> = None;
        let mut authority_acceptance_ledger = crate::fleet::AuthorityAcceptanceLedger::default();
        let mut authority_acceptance_ledger_error = None;
        if let Some(path) = authority_acceptance_ledger_path.as_deref() {
            let legacy_path = legacy_group_catalog_cache_path.as_deref().unwrap_or(path);
            match crate::fleet::load_authority_acceptance_ledger_with_legacy(path, legacy_path) {
                Ok(ledger) => authority_acceptance_ledger = ledger,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "remote group catalog history is unavailable");
                    authority_acceptance_ledger_error = Some(error);
                }
            }
        }

        let last_focus = state.active.and_then(|idx| {
            state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });

        let mut app = Self {
            config_diagnostic_deadline: None,
            toast_deadline: None,
            copy_feedback_deadline: None,
            last_api_notification_at: None,
            missing_terminal_notification_backend_warned: std::cell::Cell::new(false),
            state,
            active_overlay_client_id: None,
            pending_client_pane_focus: false,
            pane_graphics: pane_graphics::Runtime::default(),
            pane_graphics_files: Arc::new(crate::pane_graphics_files::FileStore::default()),
            direct_graphics_available: false,
            pixel_mouse_available: false,
            status_metric_refresh: crate::platform::status_metrics::StatusMetricRefresh::immediate(
                Instant::now(),
            ),
            status_metric_sampler: Arc::new(std::sync::Mutex::new(
                crate::platform::status_metrics::StatusMetricSampler::new(),
            )),
            status_metric_refresh_enabled: !cfg!(test),
            status_metrics_visible: false,
            provider_usage_refreshed_at: cfg!(test).then(Instant::now),
            provider_usage_in_flight: false,
            usage_scan_generation: 0,
            usage_scan_in_flight: None,
            connectivity_probed_at: cfg!(test).then(Instant::now),
            connectivity_probe_in_flight: false,
            terminal_runtimes: restored_terminal_runtimes,
            status_log: crate::status_log::StatusLog::for_server(),
            remote_focus_operations: remote_focus::RemoteFocusOperations::default(),
            planning_lock_key_burst: std::collections::VecDeque::new(),
            window_cycle_snapshot: None,
            fleet_attach_agents: std::collections::HashMap::new(),
            remote_focus_transport: Box::new(crate::remote::SshRemoteFocusTransport::new(
                &config.remote.fleet,
            )),
            fleet_poller_config,
            group_runtime,
            day_store_root: (!cfg!(test)).then(crate::day::default_root),
            authority_acceptance_ledger,
            authority_acceptance_ledger_path,
            authority_acceptance_ledger_error,
            authority_acceptance_ledger_writer,
            authority_acceptance_ledger_write_in_flight: false,
            pending_authority_acceptance_ledger: None,
            queued_authority_acceptance_ledger: None,
            queued_fleet_snapshot: None,
            authority_mutation_router: crate::fleet::AuthorityMutationRouter::default(),
            group_membership_projection: std::collections::BTreeMap::new(),
            #[cfg(test)]
            group_session_paths_override: None,
            git_action_panes: HashMap::new(),
            event_tx,
            event_rx,
            api_pane_state_updates: None,
            loop_history_reader,
            loop_receipt_watch_health,
            _loop_receipt_watcher: loop_receipt_watcher,
            scratchpad_watcher: None,
            scratchpad_watched_path: None,
            notepad_watcher: None,
            notepad_watched_dir: None,
            notepad_preferred_files: config.notepad.files.clone(),
            applied_notepad_config_height: Some(
                config
                    .notepad
                    .height
                    .clamp(crate::notepad::MIN_HEIGHT, crate::notepad::MAX_HEIGHT),
            ),
            notepad_git_sync: config.notepad.git_sync,
            notepad_git_sync_interval: std::time::Duration::from_secs(
                config.notepad.git_sync_interval_seconds.clamp(15, 3600),
            ),
            notepad_next_git_pull: None,
            goals_observation: None,
            goals_focused_cwd: None,
            goals_refresh_in_flight: false,
            goals_refresh_generation: 0,
            next_goals_refresh: Instant::now(),
            pomodoro_log_file: config.pomodoro.log_file.clone(),
            loop_receipt_fallback_deadline,
            loop_receipt_watch_degraded: false,
            last_git_remote_status_refresh: Instant::now() - GIT_REMOTE_STATUS_REFRESH_INTERVAL,
            last_git_repo_discovery_refresh: Instant::now(),
            git_refresh_in_flight: None,
            last_git_refresh_generation: 0,
            git_refresh_due_after_in_flight: false,
            git_identity_refresh_requested: false,
            git_status_cache: HashMap::new(),
            diff_refresh_in_flight: None,
            last_diff_refresh_generation: 0,
            files_refresh_in_flight: None,
            last_files_refresh_generation: 0,
            dock_files_refresh_demand: false,
            home_ref_refreshes_in_flight: HashSet::new(),
            git_work_context_refresh_in_flight: None,
            git_work_context_refresh_due_after_in_flight: false,
            git_work_context_rotation: 0,
            last_git_work_context_refresh_generation: 0,
            last_applied_git_work_context_refresh_generation: 0,
            next_git_work_context_refresh: Instant::now(),
            git_work_context_cache: HashMap::new(),
            git_work_context_inputs: HashMap::new(),
            work_index_config: config.work_index.clone(),
            missive_config: config.missive.clone(),
            work_index_refresh_in_flight: None,
            last_work_index_refresh_generation: 0,
            last_applied_work_index_refresh_generation: 0,
            next_work_index_refresh: Instant::now(),
            work_index_refresh_requested: false,
            work_index_context_fingerprint: Vec::new(),
            work_index_cache_bypass: crate::work_index::WorkIndexCacheBypass::default(),
            work_index_snapshot,
            work_index_session: crate::work_index::WorkIndexSession::default(),
            work_item_detail_refresh_in_flight: None,
            last_work_item_detail_refresh_generation: 0,
            last_applied_work_item_detail_refresh_generation: 0,
            foreground_process_refresh_in_flight: None,
            last_foreground_process_refresh_generation: 0,
            last_applied_foreground_process_refresh_generation: 0,
            next_foreground_process_refresh: Instant::now(),
            claude_subagent_refresh_in_flight: None,
            last_claude_subagent_refresh_generation: 0,
            last_applied_claude_subagent_refresh_generation: 0,
            next_claude_subagent_refresh: Instant::now(),
            next_claude_subagent_target_generation: 0,
            claude_subagent_refresh_rotation: 0,
            claude_subagent_trackers: HashMap::new(),
            last_status_observed: None,
            last_status_states: HashMap::new(),
            status_log_sink: status_log::StatusLogSink::default(),
            #[cfg(test)]
            git_program_override: None,
            #[cfg(test)]
            work_index_gh_program_override: None,
            #[cfg(test)]
            work_index_provider_cache_root_override: None,
            #[cfg(test)]
            work_index_linearis_program_override: None,
            #[cfg(test)]
            work_index_curl_program_override: None,
            pending_api_worktree_creates: HashMap::new(),
            pending_api_worktree_removes: HashMap::new(),
            pending_api_worktree_remove_paths: HashMap::new(),
            next_api_worktree_operation_id: 1,
            last_sidebar_divider_click: None,
            last_dock_divider_click: None,
            last_pane_click: None,
            pending_url_click_sources: HashSet::new(),
            pending_config_diagnostic_click_sources: HashSet::new(),
            pending_pomodoro_send_off_mouse_releases: HashMap::new(),
            contract_false_positive_burst_panes: HashSet::new(),
            contract_false_positive_log_path_override: None,
            next_resize_poll: Instant::now() + RESIZE_POLL_INTERVAL,
            next_auto_update_check: version_check_enabled
                .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL),
            next_agent_manifest_update_check: manifest_check_enabled
                .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL),
            update_version_check_enabled: config.update.version_check,
            update_manifest_check_enabled: config.update.manifest_check,
            loaded_host_cursor: config.ui.host_cursor,
            agent_metadata_deadline: None,
            agent_activity_refresh_deadline: None,
            pending_agent_resume_deadline: None,
            pending_agent_resume_retries: std::collections::HashMap::new(),
            pending_resume_nudges: std::collections::HashMap::new(),
            stall_nudge_episodes: std::collections::HashMap::new(),
            pending_stall_nudge_submissions: std::collections::HashMap::new(),
            session_save_deadline: None,
            session_save_scheduled_revision: None,
            session_save_thread: None,
            session_save_failures: 0,
            session_save_retry_deadline: None,
            session_dirty_since: None,
            session_save_stall_warning_at: None,
            session_writer,
            pane_exit_checkpoint_requests: HashSet::new(),
            pane_exit_checkpoint_pending: false,
            detached_custom_command_children: Vec::new(),
            detached_process_children: Vec::new(),
            tab_bar_status_generation: 0,
            tab_bar_hostname_segment: None,
            tab_bar_datetimes: Vec::new(),
            tab_bar_commands: Vec::new(),
            next_tab_bar_datetime_refresh: None,
            window_title_template: None,
            selection_autoscroll_deadline: None,
            selection_highlight_clear_deadline: None,
            persist_pane_history: config.experimental.pane_history,
            last_render_at: None,
            local_pomodoro_presentation: None,
            pending_first_frame_pane: None,
            last_presentation_at: None,
            input_leases: input::InputLeaseTable::default(),
            api_rx,
            event_hub,
            last_focus,
            status_context_focus: None,
            no_session,
            input_rx: None,
            last_terminal_size: terminal::size().ok(),
            render_notify,
            render_dirty,
            full_redraw_pending: false,
            overlay_panes: HashMap::new(),
            local_terminal_notifications: true,
            local_input_source_switch: true,
            config_reloaded_from_disk: false,
            prefix_input_source: Box::new(crate::platform::RealPrefixInputSource::default()),
        };
        app.configure_tab_bar_status(&config.ui.tab_bar_right, &config.ui.tab_bar_right_separator);
        app.configure_window_title(&config.ui.window_title);
        app.rebuild_group_membership_projection();
        if let Some(authority) = config.planning_lock.authority.clone() {
            app.state.planning_lock.fail_closed_remote();
            crate::planning_lock_sync::start_polling(authority, app.event_tx.clone());
        } else {
            let path = crate::config::config_dir().join(crate::planning_lock::CONFIG_FILE_NAME);
            crate::planning_lock_sync::watch_authority_file(path, app.event_tx.clone());
        }
        app
    }

    #[cfg(unix)]
    pub fn new_from_handoff(
        config: &Config,
        config_diagnostic: Option<String>,
        api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
        event_hub: crate::api::EventHub,
        snapshot: &crate::persist::SessionSnapshot,
        imports: &mut std::collections::HashMap<
            u32,
            crate::handoff_runtime::ImportedHandoffRuntime,
        >,
        dock_editors: &[crate::server::handoff::DockEditorHandoff],
    ) -> io::Result<Self> {
        let mut app = Self::new(config, true, config_diagnostic, api_rx, event_hub);
        let editor_imports: Vec<(
            crate::server::handoff::DockEditorHandoff,
            crate::handoff_runtime::ImportedHandoffRuntime,
        )> = dock_editors
            .iter()
            .filter_map(|editor| {
                imports
                    .remove(&editor.editor_pane_id)
                    .map(|import| (editor.clone(), import))
            })
            .collect();
        let imported_stall_nudges = imports
            .iter()
            .filter_map(|(pane_id, import)| {
                import
                    .state
                    .stall_nudge
                    .clone()
                    .map(|state| (*pane_id, state))
            })
            .collect();
        let imported_human_drafts = imports
            .iter()
            .filter_map(|(pane_id, import)| {
                import
                    .state
                    .human_draft
                    .clone()
                    .filter(|draft| !draft.is_empty())
                    .map(|draft| (*pane_id, draft))
            })
            .collect();
        let (workspaces, terminals, runtimes) = crate::persist::restore_handoff(
            snapshot,
            config.advanced.scrollback_limit_bytes,
            &config.terminal.default_shell,
            config.terminal.shell_mode,
            imports,
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )?;
        let pane_id_aliases = crate::persist::handoff_pane_aliases(snapshot, &workspaces);

        app.no_session = false;
        app.state.installed_plugins = load_plugin_registry(app.no_session);
        let now = Instant::now();
        if background_update_check_enabled(app.no_session, app.update_version_check_enabled) {
            app.next_auto_update_check = app
                .state
                .update_available
                .is_none()
                .then_some(now + AUTO_UPDATE_CHECK_INTERVAL);
        }
        if background_update_check_enabled(app.no_session, app.update_manifest_check_enabled) {
            app.next_agent_manifest_update_check = Some(now + AUTO_UPDATE_CHECK_INTERVAL);
        }
        app.state.detach_exits = false;
        app.state.workspaces = workspaces;
        app.state.refresh_local_agent_panel_identities();
        app.state.terminals = terminals;
        app.terminal_runtimes = runtimes.into();
        app.restore_stall_nudge_episodes(imported_stall_nudges, &pane_id_aliases, now);
        app.restore_handoff_human_drafts(imported_human_drafts, &pane_id_aliases);
        app.state.pane_id_aliases = pane_id_aliases;
        app.state.active = snapshot
            .active
            .filter(|&idx| idx < app.state.workspaces.len());
        app.state.selected = snapshot
            .selected
            .min(app.state.workspaces.len().saturating_sub(1));
        if let Some(width) = snapshot.sidebar_width {
            app.state.sidebar_width = width;
            app.state.sidebar_width_source = state::SidebarWidthSource::Persisted;
        }
        if let Some(split) = snapshot.sidebar_section_split {
            app.state.sidebar_section_split = split;
        }
        app.state.collapsed_space_keys = snapshot.collapsed_space_keys.clone();
        app.state.prio_panel_collapsed = snapshot.prio_panel_collapsed;
        app.state.set_server_mode(if app.state.active.is_some() {
            state::Mode::Terminal
        } else {
            state::Mode::Navigate
        });
        app.last_focus = app.state.active.and_then(|idx| {
            app.state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
        app.restore_handoff_dock_editors(config, editor_imports);
        app.rebuild_group_membership_projection();
        app.sync_agent_metadata_deadline();
        app.sync_agent_activity_refresh_deadline(now);
        Ok(app)
    }

    #[cfg(unix)]
    fn restore_handoff_dock_editors(
        &mut self,
        config: &Config,
        editor_imports: Vec<(
            crate::server::handoff::DockEditorHandoff,
            crate::handoff_runtime::ImportedHandoffRuntime,
        )>,
    ) {
        for (editor, import) in editor_imports {
            let Some(master_fd) = import.master_fd else {
                tracing::warn!(
                    pane_id = editor.editor_pane_id,
                    "dropping closed dock editor runtime during handoff restore"
                );
                continue;
            };
            let agent_pane_id = self
                .state
                .pane_id_aliases
                .get(&editor.agent_pane_id)
                .copied()
                .unwrap_or_else(|| crate::layout::PaneId::from_raw(editor.agent_pane_id));
            if self.find_pane(agent_pane_id).is_none() {
                tracing::warn!(
                    agent_pane = agent_pane_id.raw(),
                    "dropping imported dock editor whose agent pane did not restore"
                );
                continue;
            }
            let editor_pane_id = crate::layout::PaneId::alloc();
            let terminal_id = crate::terminal::TerminalId::alloc();
            let import = crate::handoff_runtime::ImportedHandoffRuntime {
                master_fd: Some(master_fd),
                state: import.state.with_pane_id(editor_pane_id),
            };
            match crate::terminal::TerminalRuntime::from_handoff_fd(
                import,
                config.advanced.scrollback_limit_bytes,
                self.state.pane_terminal_theme(),
                Some(self.state.pane_terminal_appearance()),
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ) {
                Ok(runtime) => {
                    self.terminal_runtimes.insert(terminal_id.clone(), runtime);
                    self.state.dock_editor_sessions.insert(
                        agent_pane_id,
                        state::DockEditorSession {
                            pane_id: editor_pane_id,
                            terminal_id,
                        },
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to import a dock editor runtime");
                    self.state.dock_editor_errors.insert(
                        agent_pane_id,
                        "editor did not survive the update".to_string(),
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    pub fn unpause_handoff_readers(&self) {
        self.terminal_runtimes.set_handoff_readers_paused(false);
    }

    #[cfg(unix)]
    pub fn assume_handoff_ownership(&mut self) {
        self.terminal_runtimes.assume_handoff_ownership();
    }

    fn request_repaint(&mut self) {
        self.full_redraw_pending = true;
    }

    fn focused_status_context_key(&self) -> Option<(String, usize, crate::layout::PaneId)> {
        self.state.active.and_then(|workspace_idx| {
            self.state
                .workspaces
                .get(workspace_idx)
                .and_then(|workspace| {
                    let tab_number = workspace.public_tab_number(workspace.active_tab)?;
                    workspace
                        .focused_pane_id()
                        .map(|pane_id| (workspace.id.clone(), tab_number, pane_id))
                })
        })
    }

    fn project_status_context_from_cached(&mut self) -> bool {
        let current_focus = self.focused_status_context_key();
        let changed = self.state.sync_status_focused_cached_cwd();
        self.status_context_focus = current_focus;
        changed
    }

    pub(crate) fn sync_status_focused_runtime_cwd(&mut self) -> bool {
        let current_focus = self.focused_status_context_key();
        let changed = self.state.sync_status_focused_cwd(&self.terminal_runtimes);
        self.status_context_focus = current_focus;
        changed
    }

    pub(crate) fn sync_status_context_before_render(&mut self) -> bool {
        let current_focus = self.focused_status_context_key();
        if self.state.status_focus_projection_initialized
            && current_focus == self.status_context_focus
        {
            return false;
        }
        self.project_status_context_from_cached()
    }

    pub(crate) fn record_pending_first_frame(&mut self) {
        let Some(pane_id) = self.pending_first_frame_pane else {
            return;
        };
        if self
            .state
            .view
            .pane_infos
            .iter()
            .any(|info| info.id == pane_id)
        {
            crate::logging::pane_first_frame(pane_id.raw());
            self.pending_first_frame_pane = None;
        }
    }

    pub(crate) fn sync_prefix_input_source(&mut self, previous_mode: Mode) {
        // Emit the input-source intent on entering/leaving the ASCII realm, like `ClipboardWrite`;
        // the foreground (client, or this app in monolithic mode) applies the switch. Keyed on the
        // realm so multi-level prefix commands stay ASCII. The switch is flag-gated but the restore
        // always fires on exit, so a mid-interaction flag toggle can't strand the host on ASCII.
        let next_mode = self.state.client_presentation_policy().mode();
        self.sync_prefix_input_source_modes(previous_mode, next_mode);
    }

    pub(crate) fn sync_prefix_input_source_modes(&mut self, previous_mode: Mode, next_mode: Mode) {
        let active = match (
            previous_mode.wants_ascii_input(),
            next_mode.wants_ascii_input(),
        ) {
            (false, true) if self.state.switch_ascii_input_source_in_prefix => true,
            (true, false) => false,
            _ => return,
        };
        if let Err(err) = self
            .event_tx
            .try_send(crate::events::AppEvent::PrefixInputSource {
                client_id: self.active_overlay_client_id,
                active,
            })
        {
            tracing::warn!(active, %err, "failed to queue prefix input-source change");
        }
    }

    /// Reconcile client-owned input state after runtime mutations and before
    /// view computation. Rendering must not open or close input owners.
    pub(crate) fn reconcile_client_interaction(&mut self, sync_input_source: bool) -> bool {
        let previous_mode = self.state.effective_interaction_mode();
        self.state.reconcile_client_modal_target();
        self.state.reconcile_context_menu_selection();
        let changed = self.state.effective_interaction_mode() != previous_mode;
        if sync_input_source {
            self.sync_prefix_input_source(previous_mode);
        }
        changed
    }

    /// Complete a creation or focus transition for the current presentation
    /// without changing the server-owned mode used by other attached clients.
    pub(crate) fn focus_client_on_pane(&mut self) {
        self.state.focus_client_on_pane();
        self.pending_client_pane_focus = true;
    }

    /// Set the shared default focus for future attaches without changing any
    /// currently attached client's presentation.
    pub(crate) fn focus_shared_default_on_pane(&mut self) {
        self.state.focus_client_on_pane();
        self.pending_client_pane_focus = false;
    }

    pub(crate) fn take_pending_client_pane_focus(&mut self) -> bool {
        std::mem::take(&mut self.pending_client_pane_focus)
    }

    pub(crate) fn handle_internal_event_with_prefix_sync(
        &mut self,
        event: crate::events::AppEvent,
    ) -> bool {
        let previous_mode = self.state.effective_interaction_mode();
        let changed = self.handle_internal_event_with_render_impact(event);
        self.sync_prefix_input_source(previous_mode);
        changed
    }

    #[cfg(test)]
    pub(crate) fn set_prefix_input_source(
        &mut self,
        source: Box<dyn crate::platform::PrefixInputSource>,
    ) {
        self.prefix_input_source = source;
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        if self.input_rx.is_none() {
            self.input_rx = Some(crate::raw_input::spawn_input_reader());
        }
        self.query_host_terminal_theme();

        let mut needs_render = true;
        let mut sent_window_title: Option<Option<String>> = None;
        let mut host_mouse_capture_active = self.state.mouse_capture;
        let mut host_keyboard_report_all_active = false;

        while !self.state.should_quit {
            self.reap_finished_custom_commands();
            self.reap_finished_detached_processes();
            if self.render_dirty.is_pending() {
                needs_render = true;
            }
            let terminal_title_change = self.sync_pending_terminal_titles();
            if terminal_title_change.chrome_changed
                || (terminal_title_change.raw_changed && self.terminal_title_sidebar_configured())
            {
                needs_render = true;
            }

            // Drain a bounded internal-event batch for responsiveness. API handlers
            // perform an exhaustive drain before reading pane/runtime state.
            if self.drain_internal_events() {
                needs_render = true;
            }
            if self.reconcile_remote_focus_lifecycle() {
                needs_render = true;
            }
            if self.expire_due_metadata(Instant::now()) {
                needs_render = true;
            }
            if self.drain_api_requests() {
                needs_render = true;
            }

            self.sync_focus_events();
            self.sync_session_save_schedule();

            let now = Instant::now();
            if self.handle_scheduled_tasks(now, needs_render) {
                needs_render = true;
            }

            if self.state.request_complete_onboarding {
                self.state.request_complete_onboarding = false;
                self.open_settings_from_onboarding();
                needs_render = true;
            }

            if self.state.request_new_workspace {
                self.state.request_new_workspace = false;
                self.runtime_workspace_create(
                    "tui.workspace.create",
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: None,
                        focus: true,
                        label: None,
                        env: Default::default(),
                        work_context: None,
                        source_workspace_id: None,
                    },
                );
                needs_render = true;
            }

            if let Some((ws_idx, tab_idx)) = self.state.request_pin_toggle.take() {
                self.toggle_pin_tab_via_api(ws_idx, tab_idx);
                needs_render = true;
            }

            if self.apply_pane_toggle_request() {
                needs_render = true;
            }

            if self.apply_open_repo_editor_request() {
                needs_render = true;
            }

            if self.apply_notepad_request() {
                needs_render = true;
            }

            if self.apply_git_action_request() {
                needs_render = true;
            }
            if self.apply_add_project_clone_request() {
                needs_render = true;
            }
            if self.apply_user_action_request() {
                needs_render = true;
            }
            if self.apply_save_add_action_request() {
                needs_render = true;
            }
            if self.apply_pr_command_request() {
                needs_render = true;
            }

            if self.state.request_new_tab {
                self.state.request_new_tab = false;
                let label = self.state.requested_new_tab_name.take();
                self.runtime_tab_create(
                    "tui.tab.create",
                    crate::api::schema::TabCreateParams {
                        workspace_id: None,
                        cwd: None,
                        focus: true,
                        label,
                        env: Default::default(),
                        work_context: None,
                    },
                );
                needs_render = true;
            }

            if let Some(ws_idx) = self.state.request_new_linked_worktree.take() {
                self.open_new_linked_worktree_dialog(ws_idx);
                needs_render = true;
            }

            if let Some(ws_idx) = self.state.request_open_existing_worktree.take() {
                self.open_existing_worktree_dialog(ws_idx);
                needs_render = true;
            }

            if let Some(cwd) = self.state.request_new_workspace_cwd.take() {
                self.runtime_workspace_create(
                    "tui.workspace.create_cwd",
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: Some(cwd.display().to_string()),
                        focus: true,
                        label: None,
                        env: Default::default(),
                        work_context: None,
                        source_workspace_id: None,
                    },
                );
                needs_render = true;
            }

            if let Some(ws_idx) = self.state.request_remove_linked_worktree.take() {
                self.open_remove_linked_worktree_confirmation(ws_idx);
                needs_render = true;
            }

            if self.state.request_submit_worktree_create {
                self.state.request_submit_worktree_create = false;
                self.submit_worktree_create_via_api();
                needs_render = true;
            }

            if self.state.request_submit_worktree_open {
                self.state.request_submit_worktree_open = false;
                self.submit_worktree_open_via_api();
                needs_render = true;
            }

            if self.state.request_submit_worktree_remove {
                self.state.request_submit_worktree_remove = false;
                self.submit_worktree_remove_via_api();
                needs_render = true;
            }

            if self.state.request_reload_config {
                self.state.request_reload_config = false;
                self.reload_config();
                needs_render = true;
            }

            if self.ensure_default_workspace() {
                needs_render = true;
            }

            if self.pane_graphics.retain_live_panes(&self.state) {
                needs_render = true;
            }

            let now = Instant::now();
            self.render_dirty
                .set_immediate_pty_sources(self.state.app_surface_pane_ids());
            self.sync_host_mouse_capture(&mut host_mouse_capture_active)?;
            self.sync_host_keyboard_report_all(&mut host_keyboard_report_all_active)?;
            if let Some(width) = self.state.take_dock_width_persistence_request() {
                crate::client::presentation::save_dock_width(width);
            }
            if let Some(height) = self.state.take_notepad_height_persistence_request() {
                crate::client::presentation::save_notepad_height(height);
            }
            if let Some(mode) = self.state.take_sidebar_group_mode_persistence_request() {
                crate::client::presentation::save_sidebar_group_mode(mode);
            }
            if let Some((key, mode)) = self.state.take_sidebar_group_sort_persistence_request() {
                crate::client::presentation::save_sidebar_group_sort(&key, mode);
            }
            if let Some((key, collapsed)) = self
                .state
                .take_sidebar_group_collapsed_persistence_request()
            {
                crate::client::presentation::save_sidebar_group_collapsed(&key, collapsed);
            }
            if self.state.take_sidebar_view_scan_request() {
                self.request_sidebar_view_scan(now);
            }
            if let Some(filter) = self.state.take_sidebar_work_filter_persistence_request() {
                crate::client::presentation::save_sidebar_work_filter(filter);
            }

            if needs_render && self.can_render_now(now) {
                self.reconcile_client_interaction(true);
                self.sync_status_context_before_render();
                let _ = self.render_dirty.take();
                if self.window_title_configured() {
                    let title = self
                        .window_title()
                        .and_then(|title| crate::config::sanitize_window_title_text(&title));
                    if sent_window_title.as_ref() != Some(&title) {
                        crate::terminal_effects::write_window_title(
                            &mut std::io::stdout(),
                            title.as_deref(),
                        )?;
                        sent_window_title = Some(title);
                    }
                }
                let _sync_output = SyncOutputGuard::begin()?;
                let kitty_graphics_enabled = self.state.kitty_graphics_enabled;
                if self.full_redraw_pending {
                    for cell in &mut terminal.current_buffer_mut().content {
                        cell.set_skip(true);
                    }
                    terminal.swap_buffers();
                    self.full_redraw_pending = false;
                }
                let mut cell_size = self.state.host_cell_size;
                terminal.draw(|frame| {
                    let area = frame.area();
                    if let Some(observed_cell_size) =
                        crate::kitty_graphics::HostCellSize::try_from_terminal(area)
                    {
                        self.state.host_cell_size = observed_cell_size;
                        cell_size = observed_cell_size;
                    } else if kitty_graphics_enabled && !cell_size.is_known() {
                        cell_size = crate::kitty_graphics::HostCellSize::fallback_for_area(area);
                    }
                    crate::ui::compute_view_with_cell_size(
                        &mut self.state,
                        &self.terminal_runtimes,
                        area,
                        cell_size,
                    );
                    self.ensure_dock_editor();
                    self.resize_dock_editor();
                    self.ensure_scratchpad();
                    self.ensure_notepad();
                    crate::ui::render_with_runtime_registry(
                        &self.state,
                        &self.terminal_runtimes,
                        frame,
                    );
                })?;
                self.status_metrics_visible =
                    self.state.view.status_bar_rect != ratatui::layout::Rect::default();
                if kitty_graphics_enabled {
                    crate::kitty_graphics::paint_local_pane_graphics(
                        &self.state,
                        &self.pane_graphics,
                        &self.terminal_runtimes,
                        cell_size,
                    )?;
                }
                self.status_metrics_visible =
                    self.state.view.status_bar_rect != ratatui::layout::Rect::default();
                self.local_pomodoro_presentation =
                    Some(crate::ui::pomodoro::input_presentation_at(
                        &self.state,
                        self.state.screen_rect(),
                        self.state.view_observed_at,
                    ));
                self.sync_pending_agent_resume_deadline(now);
                if self.start_pending_agent_resumes_at(self.pending_agent_resume_due(now), now) {
                    self.render_dirty.request_generic();
                    self.render_notify.notify_one();
                }
                self.last_render_at = Some(now);
                self.sync_agent_activity_refresh_deadline(self.state.view_observed_at);
                needs_render = false;
                continue;
            }

            let next_deadline = self.next_loop_deadline(now, needs_render);
            let event = {
                let input_rx = self.input_rx.as_mut();
                tokio::select! {
                    maybe_api = self.api_rx.recv() => match maybe_api {
                        Some(msg) => LoopEvent::Api(Box::new(msg)),
                        None => LoopEvent::Timer,
                    },
                    maybe_ev = self.event_rx.recv() => match maybe_ev {
                        Some(ev) => LoopEvent::Internal(ev),
                        None => LoopEvent::Timer,
                    },
                    maybe_input = recv_raw_input_or_pending(input_rx) => match maybe_input {
                        Some(input) => LoopEvent::RawInput(input),
                        None => LoopEvent::InputClosed,
                    },
                    _ = sleep_until_or_pending(next_deadline) => LoopEvent::Timer,
                    _ = self.render_notify.notified() => LoopEvent::RenderRequested,
                }
            };

            match event {
                LoopEvent::Timer => {}
                LoopEvent::Internal(ev) => {
                    if self.handle_internal_event_with_prefix_sync(ev) {
                        needs_render = true;
                    }
                }
                LoopEvent::Api(msg) => {
                    if self.handle_api_request_message(*msg) {
                        needs_render = true;
                    }
                }
                LoopEvent::RawInput(input) => {
                    if self.handle_raw_input_batch(input).await {
                        needs_render = true;
                    }
                }
                LoopEvent::InputClosed => {
                    self.input_rx = None;
                }
                LoopEvent::RenderRequested => {
                    if self.render_dirty.is_pending() {
                        needs_render = true;
                    }
                }
            }
        }

        // Save session on exit (skip in --no-session mode)
        if !self.no_session {
            self.save_session_now();
        }

        Ok(())
    }

    fn sync_host_keyboard_report_all(&self, active: &mut bool) -> io::Result<()> {
        let desired = self.host_keyboard_report_all_requested();
        if desired == *active {
            return Ok(());
        }
        crate::terminal_modes::set_host_kitty_keyboard_report_all(&mut io::stdout(), desired)?;
        *active = desired;
        Ok(())
    }

    fn sync_host_mouse_capture(&self, active: &mut bool) -> io::Result<()> {
        let desired = self
            .state
            .should_capture_host_mouse_from(&self.terminal_runtimes);
        if desired == *active {
            return Ok(());
        }
        crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        if desired {
            execute!(io::stdout(), EnableMouseCapture)?;
        } else {
            execute!(io::stdout(), DisableMouseCapture)?;
        }
        *active = desired;
        Ok(())
    }

    pub(crate) fn ensure_default_workspace(&mut self) -> bool {
        if !self.state.workspaces.is_empty()
            || self.state.server_mode() == Mode::Onboarding
            || self.state.pending_workspace_create_cwd.is_some()
        {
            return false;
        }

        let previous_mode = self.state.server_mode();
        let preserve_mode = matches!(
            previous_mode,
            Mode::ReleaseNotes | Mode::ProductAnnouncement | Mode::Settings
        );
        let cwd = self.resolve_new_terminal_cwd(None);
        let preserve_checkpoint = self.pane_exit_checkpoint_pending && !self.state.session_dirty;

        match self.create_workspace_with_options(cwd, true) {
            Ok(_) => {
                if preserve_checkpoint {
                    self.pane_exit_checkpoint_pending = true;
                    self.finish_checkpointed_pane_exit();
                }
                if preserve_mode {
                    self.state.set_server_mode(previous_mode);
                }
                true
            }
            Err(err) => {
                tracing::error!(err = %err, "failed to create default workspace");
                self.state.set_server_mode(Mode::Navigate);
                false
            }
        }
    }

    pub(crate) fn dismiss_release_notes(&mut self) {
        let preview = self
            .state
            .release_notes
            .as_ref()
            .is_some_and(|notes| notes.preview);

        self.state.release_notes = None;
        if !preview {
            if let Err(err) = crate::release_notes::mark_current_version_seen() {
                self.state.config_diagnostic =
                    Some(format!("failed to update release notes status: {err}"));
                self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(5));
            }
        }

        if self.state.product_announcement.is_some() {
            self.state.set_server_mode(Mode::ProductAnnouncement);
        } else {
            self.state.set_server_mode(if self.state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            });
        }
    }

    pub(crate) fn dismiss_product_announcement(&mut self) {
        if let Some(announcement) = self.state.product_announcement.take() {
            if !announcement.preview {
                if let Err(err) =
                    crate::product_announcements::mark_seen(&announcement.version, &announcement.id)
                {
                    self.state.config_diagnostic =
                        Some(format!("failed to update announcement status: {err}"));
                    self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }

        self.state.set_server_mode(if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        });
    }

    pub(crate) fn scroll_release_notes(&mut self, delta: i16) {
        let max_scroll = self.state.release_notes_max_scroll();
        if let Some(notes) = &mut self.state.release_notes {
            notes.scroll = if delta.is_negative() {
                notes.scroll.saturating_sub(delta.unsigned_abs())
            } else {
                notes.scroll.saturating_add(delta as u16)
            }
            .min(max_scroll);
        }
    }

    pub(crate) fn scroll_product_announcement(&mut self, delta: i16) {
        let max_scroll = self.state.product_announcement_max_scroll();
        if let Some(announcement) = &mut self.state.product_announcement {
            announcement.scroll = if delta.is_negative() {
                announcement.scroll.saturating_sub(delta.unsigned_abs())
            } else {
                announcement.scroll.saturating_add(delta as u16)
            }
            .min(max_scroll);
        }
    }

    pub(crate) fn open_settings_from_onboarding(&mut self) {
        self.mark_onboarding_complete();
        self.refresh_integration_recommendations();
        crate::app::input::open_settings_at(&mut self.state, state::SettingsSection::Integrations);
    }

    pub(crate) fn refresh_integration_recommendations(&mut self) {
        self.state.integration_recommendations = crate::integration::integration_recommendations();
    }

    pub(crate) fn install_recommended_integrations(&mut self) {
        let targets = self
            .state
            .integration_recommendations
            .iter()
            .filter(|recommendation| recommendation.needs_install())
            .map(|recommendation| recommendation.target)
            .collect::<Vec<_>>();

        self.state.integration_install_messages.clear();
        if targets.is_empty() {
            self.state
                .integration_install_messages
                .push("all detected integrations are current".to_string());
            return;
        }

        for target in targets {
            let label = crate::integration::integration_target_label(target);
            match crate::integration::install_target(target) {
                Ok(messages) => {
                    self.state
                        .integration_install_messages
                        .push(format!("installed {label}"));
                    self.state
                        .integration_install_messages
                        .extend(messages.into_iter().filter(|message| {
                            message.starts_with(crate::integration::INSTALL_WARNING_PREFIX)
                        }));
                }
                Err(err) => self
                    .state
                    .integration_install_messages
                    .push(format!("{label}: {err}")),
            }
        }

        self.state.integration_recommendations = crate::integration::integration_recommendations();
        self.state.mark_session_dirty();
    }

    pub(crate) fn reload_config(&mut self) -> crate::config::ConfigReloadReport {
        self.apply_config_from_disk(true)
    }

    pub(crate) fn take_config_reloaded_from_disk(&mut self) -> bool {
        let reloaded = self.config_reloaded_from_disk;
        self.config_reloaded_from_disk = false;
        reloaded
    }

    pub(crate) fn apply_config_from_disk(
        &mut self,
        notify_success: bool,
    ) -> crate::config::ConfigReloadReport {
        self.config_reloaded_from_disk = true;
        let previous_toast = self.state.toast.clone();
        let report = match crate::config::load_live_config() {
            Ok(loaded) => self.apply_live_config(
                &loaded.config,
                &loaded.diagnostics,
                &loaded.invalid_sections,
                notify_success,
            ),
            Err(diagnostics) => {
                self.state.toast = None;
                self.state.config_diagnostic =
                    crate::config::config_diagnostic_summary(&diagnostics);
                self.config_diagnostic_deadline = None;
                crate::config::ConfigReloadReport {
                    status: crate::config::ConfigReloadStatus::Failed,
                    diagnostics,
                }
            }
        };
        self.sync_toast_deadline(previous_toast);
        report
    }

    fn apply_live_config(
        &mut self,
        config: &crate::config::Config,
        load_diagnostics: &[String],
        invalid_sections: &[String],
        notify_success: bool,
    ) -> crate::config::ConfigReloadReport {
        let mut diagnostics = load_diagnostics.to_vec();
        let invalid_section =
            |section: &str| invalid_sections.iter().any(|invalid| invalid == section);

        if !invalid_section("agent_detection") {
            self.state.full_lifecycle_hook_authority_timeout = std::time::Duration::from_secs(
                config
                    .agent_detection
                    .full_lifecycle_hook_authority_timeout_seconds,
            );
        }

        if !invalid_section("session") {
            let hide_done_after = std::time::Duration::from_secs(
                config.session.hide_done_after_minutes.saturating_mul(60),
            );
            let hide_threshold_changed = self.state.hide_done_after != hide_done_after;
            self.state.hide_done_after = hide_done_after;
            self.state.reap_done_after = std::time::Duration::from_secs(
                config.session.reap_done_after_minutes.saturating_mul(60),
            );
            self.state.reap_done_panes = config.session.reap_done_panes;
            self.state.auto_settle_finished = config.session.auto_settle_finished;
            self.state.settle_finished_after = std::time::Duration::from_secs(
                config
                    .session
                    .settle_finished_after_minutes
                    .saturating_mul(60),
            );
            self.state.auto_settle_inactive = config.session.auto_settle_inactive;
            self.state.auto_settle_done = config.session.auto_settle_done;
            self.state.settle_done_after = std::time::Duration::from_secs(
                config.session.settle_done_after_minutes.saturating_mul(60),
            );
            self.state.settle_stops_agent = config.session.settle_stops_agent;
            self.state.settled_read_only_after = std::time::Duration::from_secs(
                config
                    .session
                    .settled_read_only_after_minutes
                    .saturating_mul(60),
            );
            self.state.nudge_resumed_agents = config.session.nudge_resumed_agents;
            self.state
                .resume_nudge_message
                .clone_from(&config.session.resume_nudge_message);
            self.state.auto_nudge_stalled_agents = config.session.auto_nudge_stalled_agents;
            self.state.agent_stale_after =
                auto_nudge::nudge_after_duration(config.session.agent_stale_after_minutes);
            self.state.agent_subagent_stale_after =
                auto_nudge::nudge_after_duration(config.session.agent_subagent_stale_after_minutes);
            self.state.nudge_after =
                auto_nudge::nudge_after_duration(config.session.nudge_after_minutes);
            self.state.max_nudges = config.session.max_nudges;
            self.state.max_nudges_without_human = config.session.max_nudges_without_human;
            self.state
                .stall_nudge_message
                .clone_from(&config.session.stall_nudge_message);
            self.state.settle_after = std::time::Duration::from_secs(
                config
                    .session
                    .settle_after_days
                    .saturating_mul(24 * 60 * 60),
            );
            if hide_threshold_changed {
                self.state.mark_sidebar_projection_changed();
            }
        }

        if !invalid_section("keys") {
            match config.live_keybinds_with_diagnostics() {
                Ok((mut live, keybind_diagnostics)) => {
                    if invalid_section("actions") {
                        live.keybinds.user_actions = self.state.keybinds.user_actions.clone();
                    }
                    self.state.prefix_code = live.prefix.0;
                    self.state.prefix_mods = live.prefix.1;
                    self.state.keybinds = live.keybinds;
                    diagnostics.extend(keybind_diagnostics);
                }
                Err(keybind_diagnostics) => {
                    diagnostics.extend(
                        keybind_diagnostics
                            .into_iter()
                            .map(|diagnostic| format!("{diagnostic}; kept current keybinds")),
                    );
                }
            }
        }

        if !invalid_section("panel") {
            self.state.dock_default_surfaces = dock_surfaces_from_config(&config.panel);
        }

        if !invalid_section("remote") {
            let revoked_operations = self
                .remote_focus_transport
                .reload_fleet(&config.remote.fleet);
            let config_generation = self
                .fleet_poller_config
                .replace(config.remote.fleet.clone());
            self.authority_mutation_router
                .reconfigure(config_generation);
            let reconciled_snapshot = self
                .state
                .fleet_snapshot
                .reconcile_after_config_reload(&config.remote.fleet, config_generation);
            let catalogs_changed =
                self.state.fleet_snapshot.group_catalogs != reconciled_snapshot.group_catalogs;
            self.authority_mutation_router
                .observe_snapshot(&reconciled_snapshot);
            self.state.fleet_snapshot = reconciled_snapshot;
            if catalogs_changed {
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::AuthorityCatalogsUpdated,
                    data: crate::api::schema::EventData::AuthorityCatalogsUpdated {
                        catalogs: self.authority_catalog_infos(),
                    },
                });
            }
            self.state.reconcile_dock_hosts_selection();
            self.refresh_remote_agent_panel_entries();
            for operation_id in revoked_operations {
                self.apply_remote_focus_transition(
                    &operation_id,
                    remote_focus::RemoteFocusTransition::Closed,
                );
            }
            let agent_host_name = config.remote.fleet.resolved_self_name();
            self.state.chrome_host_label = config
                .remote
                .fleet
                .chrome_host_label_for_resolved_name(&agent_host_name);
            let tab_bar_host_label =
                crate::app::tab_bar_status::sanitize_status_text(&self.state.chrome_host_label);
            if let Some(index) = self.tab_bar_hostname_segment {
                if let Some(state::TabBarStatusSegment::Text(value)) =
                    self.state.tab_bar_right.get_mut(index)
                {
                    *value = tab_bar_host_label;
                }
            }
            if let Some((template, hostname)) = self.window_title_template.as_mut() {
                if template.uses(crate::config::WindowTitleToken::Hostname) {
                    hostname.clone_from(&self.state.chrome_host_label);
                }
            }
            if self.state.agent_host_name != agent_host_name {
                self.state.agent_host_name = agent_host_name;
                self.refresh_remote_agent_panel_entries();
                self.state.mark_sidebar_projection_changed();
            }
        }

        if !invalid_section("linear") {
            self.state.linear_default_layout = config.linear.default_layout.into();
        }

        if !invalid_section("ui") {
            // Validate sidebar bounds before they reach any `u16::clamp` call.
            // On `min > max`, treat the entire `[ui]` section as invalid: keep
            // the previous settings and skip the section so the re-clamp below
            // — and every subsequent render/drag — can never panic.
            if let Some(diagnostic) = config.invalid_sidebar_bounds_diagnostic() {
                diagnostics.push(format!("{diagnostic}; keeping previous [ui] settings"));
            } else {
                diagnostics.extend(config.ui.sound.diagnostics());
                diagnostics.extend(crate::config::tab_bar_right_diagnostics(
                    &config.ui.tab_bar_right,
                ));
                diagnostics.extend(crate::config::window_title_diagnostics(
                    &config.ui.window_title,
                ));

                self.state.default_sidebar_width = config.ui.sidebar_width;
                if self.state.sidebar_width_source == state::SidebarWidthSource::ConfigDefault {
                    self.state.sidebar_width = config.ui.sidebar_width;
                }
                self.state.sidebar_min_width = config.ui.sidebar_min_width;
                self.state.sidebar_max_width = config.ui.sidebar_max_width;
                self.state.sidebar_collapsed_mode = config.ui.sidebar_collapsed_mode;
                self.state.window_cycle_mode = config.ui.window_cycle_mode;
                self.state.skip_collapsed_cycle = config.ui.skip_collapsed_cycle;
                self.state
                    .hyperspace
                    .set_enabled(config.ui.sidebar_animation, Instant::now());
                if !self.state.hyperspace.enabled {
                    // Drop the hidden box's geometry now so its pause edge stops
                    // taking clicks before the next view pass reflows the footer.
                    self.state.view.hyperspace_rect = Rect::default();
                    self.state.view.hyperspace_pause_hit_area = Rect::default();
                }
                self.state.mobile_width_threshold = config.ui.mobile_width_threshold;
                // Re-clamp the live width to the new bounds. No source guard — bounds
                // always apply, including to widths owned by Persisted or Manual.
                self.state.sidebar_width = self
                    .state
                    .sidebar_width
                    .clamp(self.state.sidebar_min_width, self.state.sidebar_max_width);
                self.state.mouse_capture = config.ui.mouse_capture;
                self.state.copy_on_select = config.ui.copy_on_select;
                if self.state.redraw_on_focus_gained != config.ui.redraw_on_focus_gained {
                    self.state.request_client_config_reload = true;
                }
                self.state.redraw_on_focus_gained = config.ui.redraw_on_focus_gained;
                if self.loaded_host_cursor != config.ui.host_cursor {
                    self.state.request_client_config_reload = true;
                }
                self.loaded_host_cursor = config.ui.host_cursor;
                self.state.mouse_scroll_lines = config.ui.mouse_scroll_lines();
                self.state.right_click_passthrough_modifiers =
                    config.ui.right_click_passthrough_modifiers();
                self.state.confirm_close = config.ui.confirm_close;
                if self.state.combine_repos_across_hosts != config.ui.combine_repos_across_hosts {
                    self.state.combine_repos_across_hosts = config.ui.combine_repos_across_hosts;
                    self.state.mark_sidebar_projection_changed();
                }
                if self.state.dock_diff_ignore_whitespace != config.ui.hide_whitespace_in_diff {
                    self.state.dock_diff_ignore_whitespace = config.ui.hide_whitespace_in_diff;
                    self.state.invalidate_dock_diff();
                }
                self.state.new_thread_workspace = config.ui.new_thread_workspace;
                self.state.launch_profiles =
                    crate::app::launch_profiles::resolve(&config.launch_profiles);
                self.state.projects = crate::app::projects::resolve(&config.projects);
                self.state.machines = crate::app::machines::resolve(&config.remote.fleet);
                self.state
                    .add_project_start_dir
                    .clone_from(&config.ui.add_project_start_dir);
                self.state.prompt_new_tab_name = config.ui.prompt_new_tab_name;
                self.state.prompt_new_workspace_name = config.ui.prompt_new_workspace_name;
                self.state.pane_borders = config.ui.pane_borders;
                self.state.pane_scrollbars = config.ui.pane_scrollbars;
                self.state.show_pull_button = config.ui.show_pull_button;
                // The button is the Git menu's only anchor and entry point.
                // Close it at the config transition, never during view computation.
                if self.state.server_mode() == Mode::GitMenu && !self.state.show_pull_button {
                    self.state.set_server_mode(Mode::Terminal);
                }
                self.state.open_dock_on_work_link = config.ui.open_dock_on_work_link;
                self.state.show_pane_toggle_buttons = config.ui.show_pane_toggle_buttons;
                self.state.pane_gaps = config.ui.pane_gaps;
                self.state.show_agent_labels_on_pane_borders =
                    config.ui.show_agent_labels_on_pane_borders;
                self.state.hide_tab_bar_when_single_tab = config.ui.hide_tab_bar_when_single_tab;
                self.state.status_bar_expanded = config.ui.status_bar_expanded;
                self.state.fleet_status = config.ui.fleet_status;
                let status_bar_was_enabled = self.state.status_bar_enabled;
                self.state.status_bar_enabled = config.ui.status_bar.enabled;
                if self.state.status_bar_enabled && !status_bar_was_enabled {
                    self.project_status_context_from_cached();
                    self.state.status_git_cwd = None;
                    self.state.status_git_branch = None;
                    self.request_git_identity_refresh(Instant::now());
                }
                let agent_panel_sort = agent_panel_sort_from_config(config.ui.agent_panel_sort);
                let mut sidebar_projection_changed = false;
                if self.state.agent_panel_sort != agent_panel_sort {
                    self.state.agent_panel_sort = agent_panel_sort;
                    sidebar_projection_changed = true;
                }
                if self.state.sidebar_agents != config.ui.sidebar.agents
                    || self.state.sidebar_spaces != config.ui.sidebar.spaces
                    || self.state.sidebar_areas != config.ui.sidebar.areas
                    || self.state.sidebar_sections_layout
                        != (config.ui.sidebar.layout
                            == crate::config::SidebarLayoutConfig::Sections)
                    || self.state.sidebar_show_ask_subtitles != config.ui.sidebar.show_ask_subtitles
                {
                    sidebar_projection_changed = true;
                }
                self.state.tab_bar_position = config.ui.tab_bar_position;
                self.state.agent_panel_sort =
                    agent_panel_sort_from_config(config.ui.agent_panel_sort);
                self.state.status_indicators = config.ui.status_indicators;
                self.state.working_row_opacity_percent = config.ui.working_row_opacity_percent;
                self.state.sidebar_agents = config.ui.sidebar.agents.clone();
                self.state.sidebar_spaces = config.ui.sidebar.spaces.clone();
                self.state.sidebar_areas = config.ui.sidebar.areas.clone();
                self.state.sidebar_sections_layout =
                    config.ui.sidebar.layout == crate::config::SidebarLayoutConfig::Sections;
                self.state.sidebar_show_ask_subtitles = config.ui.sidebar.show_ask_subtitles;
                if self.state.sidebar_sections_layout && !self.state.sidebar_areas.notes {
                    self.state.set_notepad_focus(false);
                }
                self.state.sidebar_header_plain =
                    config.ui.sidebar.header == crate::config::SidebarHeaderConfig::Plain;
                if sidebar_projection_changed {
                    self.state.mark_sidebar_projection_changed();
                }
                self.state.accent = crate::config::parse_color(&config.ui.accent);
                if !self.state.local_sound_playback
                    && self.state.request_client_notification_config.is_none()
                    && self.state.sound != config.ui.sound
                {
                    self.state.request_client_config_reload = true;
                }
                self.state.sound = config.ui.sound.clone();
                if self.state.toast_config.terminal_backend != config.ui.toast.terminal_backend {
                    self.state.request_client_config_reload = true;
                }
                if config.ui.toast.delivery != crate::config::ToastDelivery::Off {
                    self.state.last_non_off_toast_delivery = config.ui.toast.delivery;
                }
                self.state.toast_config = config.ui.toast.clone();
            }
        }

        // Their own gates: these sidebar companions read nothing out of `[ui]`,
        // so a broken `[ui]` section must not freeze any of them.
        if !invalid_section("notepad") {
            self.apply_notepad_config(&config.notepad);
        }
        if !invalid_section("goals_panel") {
            self.apply_goals_config(&config.goals_panel);
        }
        if !invalid_section("pomodoro") {
            self.apply_pomodoro_config(&config.pomodoro);
        }

        if !invalid_section("experimental") {
            let was_kitty_graphics_enabled = self.state.kitty_graphics_enabled;
            self.state.kitty_graphics_enabled = config.experimental.kitty_graphics;
            crate::kitty_graphics::set_enabled(config.experimental.kitty_graphics);
            if was_kitty_graphics_enabled && !config.experimental.kitty_graphics {
                let _ = crate::kitty_graphics::clear_all_host_graphics();
                self.pane_graphics.clear();
            }
            self.state.reveal_hidden_cursor_for_cjk_ime =
                config.experimental.reveal_hidden_cursor_for_cjk_ime;
            self.state.cjk_ime_agent_filter_configured =
                !config.experimental.cjk_ime_agents.is_empty();
            self.state.cjk_ime_agents = parse_cjk_ime_agents(&config.experimental.cjk_ime_agents);
            self.state.cjk_ime_cursor_shape =
                config.experimental.cjk_ime_cursor_shape.to_decscusr();
            self.state.switch_ascii_input_source_in_prefix =
                config.experimental.switch_ascii_input_source_in_prefix;
            self.persist_pane_history = config.experimental.pane_history;
            self.state.pane_history_persistence = config.experimental.pane_history;
            if !self.persist_pane_history {
                crate::persist::clear_history();
            }
        }

        if !invalid_section("server") {
            if let Some(diagnostic) = config.invalid_headless_size_diagnostic() {
                diagnostics.push(format!("{diagnostic}; keeping current [server] settings"));
            } else {
                self.state.headless_size = config.headless_size();
            }
        }

        if !invalid_section("advanced") {
            self.state.pane_scrollback_limit_bytes = config.advanced.scrollback_limit_bytes;
        }

        if !invalid_section("update") {
            let now = Instant::now();
            let previous_version_check_enabled = self.update_version_check_enabled;
            let previous_manifest_check_enabled = self.update_manifest_check_enabled;
            self.update_version_check_enabled = config.update.version_check;
            self.update_manifest_check_enabled = config.update.manifest_check;

            if !self.update_version_check_enabled {
                self.next_auto_update_check = None;
            } else if !previous_version_check_enabled
                && background_update_check_enabled(
                    self.no_session,
                    self.update_version_check_enabled,
                )
                && self.state.update_available.is_none()
            {
                self.next_auto_update_check = Some(now);
            }

            if !self.update_manifest_check_enabled {
                self.next_agent_manifest_update_check = None;
            } else if !previous_manifest_check_enabled
                && background_update_check_enabled(
                    self.no_session,
                    self.update_manifest_check_enabled,
                )
            {
                self.next_agent_manifest_update_check = Some(now);
            }
        }

        if !invalid_section("terminal") {
            self.state.default_shell = config.terminal.default_shell.clone();
            self.state.shell_mode = config.terminal.shell_mode;
            self.state.new_terminal_cwd = config.terminal.new_cwd.clone();
        }

        if !invalid_section("worktrees") {
            self.state.worktree_directory =
                crate::worktree::expand_tilde_absolute_path(&config.worktrees.directory);
        }

        if !invalid_section("files") {
            self.state.files_icons = config.files.icons;
        }
        if !invalid_section("ui") {
            let nerd_font_changed = self.state.nerd_font != config.ui.nerd_font;
            self.state.nerd_font = config.ui.nerd_font;
            self.state.space_icons = config.ui.icons.spaces.clone();
            if nerd_font_changed {
                self.refresh_remote_agent_panel_entries();
            }
        }

        if !invalid_section("usage") {
            self.state.usage_pricing = config.usage.clone();
        }

        if !invalid_section("source_control") {
            self.state.pr_merge_method = config.source_control.merge_method;
            self.state.branch_prefix = config.source_control.branch_prefix.clone();
            self.state.commit_message_model = config.source_control.commit_message_model.clone();
            self.state.commit_stage_all = config.source_control.commit_stage_all;
        }

        if !invalid_section("work_index") {
            self.work_index_config = config.work_index.clone();
            self.state.work_index_enabled = self.work_index_config.enabled;
            if let Some(view) = self.state.work_view.as_mut() {
                view.enabled = self.work_index_config.enabled;
                if !view.enabled {
                    view.snapshot = None;
                }
            }
            if !self.work_index_config.enabled {
                self.work_index_refresh_in_flight = None;
            }
        }

        if !invalid_section("day_board") {
            self.state.day_stale_after = Duration::from_secs(config.day_board.stale_after);
        }

        if !invalid_section("missive") {
            self.missive_config = config.missive.clone();
            self.state.settings_missive_team = config.missive.team.clone();
            self.state.settings_missive_token_present =
                std::env::var_os(&config.missive.token_env).is_some_and(|value| !value.is_empty());
            self.next_work_index_refresh = Instant::now();
        }

        if !invalid_section("theme") {
            // A reload re-reads config, and config alone cannot know the host
            // appearance on a relay that drops OSC 11 answers. Rebuilding the
            // runtime wholesale therefore threw away whatever `herdr theme set`
            // or the attach relay had asserted, and the chrome fell back to
            // `[theme] name` until the next push.
            let asserted = self.state.theme_runtime.runtime_host_appearance;
            self.state.theme_runtime = theme_runtime_config(config, !invalid_section("ui"));
            if let Some(appearance) = asserted {
                self.state.theme_runtime.runtime_host_appearance = Some(appearance);
                // A pinned config value still wins, matching how the client
                // resolves config against the environment.
                if self.state.theme_runtime.host_appearance
                    == crate::config::HostAppearanceOverride::Auto
                {
                    self.state.theme_runtime.host_appearance = appearance;
                }
            }
            self.refresh_effective_app_theme();
        }

        let status = if diagnostics.is_empty() {
            crate::config::ConfigReloadStatus::Applied
        } else {
            crate::config::ConfigReloadStatus::Partial
        };

        if diagnostics.is_empty() {
            self.state.config_diagnostic = None;
            self.config_diagnostic_deadline = None;
            if notify_success {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::UpdateInstalled,
                    title: "reloaded config".to_string(),
                    context: "using config.toml".to_string(),
                    position: None,
                    target: None,
                });
            }
        } else {
            self.state.config_diagnostic = crate::config::config_diagnostic_summary(&diagnostics);
            self.config_diagnostic_deadline = None;
            if notify_success {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::UpdateInstalled,
                    title: "reloaded config".to_string(),
                    context: "with warnings".to_string(),
                    position: None,
                    target: None,
                });
            }
        }

        // The mismatch is a warning about the terminal in front of the config,
        // not a defect in the config itself: report it without demoting a valid
        // reload to `Partial` or claiming the config diagnostic banner.
        if let Some(mismatch) = &self.state.theme_appearance_mismatch {
            diagnostics.push(mismatch.clone());
        }

        crate::config::ConfigReloadReport {
            status,
            diagnostics,
        }
    }

    #[cfg(test)]
    pub(crate) fn apply_live_config_for_test(&mut self, config: &crate::config::Config) {
        self.apply_live_config(config, &[], &[], false);
    }
}

// ---------------------------------------------------------------------------
// Input routing for headless server mode
// ---------------------------------------------------------------------------

impl App {
    pub(crate) fn terminal_input_context(&self) -> Option<TerminalInputContext> {
        // Full-frame overlays bypass ordinary pane context. The inbox routes keys
        // to a selected pane, while Symphony and home consume them themselves.
        if self.state.symphony_detail.is_some()
            || self.state.loop_run_history_detail.is_some()
            || self.state.aloop_run_detail.is_some()
            || self.state.work_view.is_some()
            || self.state.usage_view.is_some()
            || self.state.inbox.is_some()
            || self.state.home.is_some()
        {
            None
        } else if let Some(popup) = &self.state.popup_pane {
            Some(TerminalInputContext::Popup(popup.terminal_id.clone()))
        } else if self.state.effective_interaction_mode() == Mode::Terminal {
            Some(TerminalInputContext::Pane)
        } else {
            None
        }
    }

    pub(crate) fn focused_remote_proxy_input_gate_closed(&self) -> bool {
        if !matches!(
            self.terminal_input_context(),
            Some(TerminalInputContext::Pane)
        ) {
            return false;
        }
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        self.state
            .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            .and_then(|runtime| runtime.remote_proxy_input_enabled())
            .is_some_and(|enabled| !enabled)
    }

    fn execute_repeat_plan_headless(
        &mut self,
        source_id: InputSourceId,
        lease_key: input::InputLeaseKey,
        key: crate::input::TerminalKey,
        plan: input::RepeatPlan,
        before_terminal_input: &mut impl FnMut(&TerminalInputTarget),
        controlled_owners: Option<&std::collections::HashMap<crate::terminal::TerminalId, u64>>,
    ) {
        match plan {
            input::RepeatPlan::Forwarded(target) => {
                if !self.forward_terminal_key_to_target_headless(&target, key) {
                    self.input_leases.remove(&lease_key);
                }
            }
            input::RepeatPlan::Reprocess {
                context,
                repetitions,
                tracked,
            } => {
                let key = key
                    .with_kind(crossterm::event::KeyEventKind::Repeat)
                    .with_repeat_count(1);
                let mut forwarded_target = None;
                for _ in 0..repetitions {
                    if let Some(target) = &forwarded_target {
                        if !self.forward_terminal_key_to_target_headless(target, key.clone()) {
                            self.input_leases.remove(&lease_key);
                            break;
                        }
                        continue;
                    }
                    let current_context = self.terminal_input_context();
                    if !self.input_leases.reprocess_allowed(
                        lease_key,
                        &context,
                        current_context.as_ref(),
                        tracked,
                    ) {
                        break;
                    }
                    if let Some(target) = self.handle_terminal_key_headless_from_with_hook(
                        source_id,
                        key.clone(),
                        before_terminal_input,
                        controlled_owners,
                    ) {
                        if tracked {
                            self.input_leases.insert_forwarded(
                                lease_key,
                                target.clone(),
                                key.clone(),
                            );
                            forwarded_target = Some(target);
                        }
                    }
                }
            }
            input::RepeatPlan::Ignore => {}
        }
    }

    /// Routes raw input bytes from a client through the existing input pipeline.
    ///
    /// The input bytes are parsed into `RawInputEvent`s and then processed.
    /// In terminal mode, keys are routed through the same semantic
    /// key-handling path as monolithic herdr so they are re-encoded for the
    /// focused pane's negotiated keyboard protocol instead of passing host
    /// terminal escape sequences through unchanged.
    #[cfg(test)]
    pub(crate) fn route_client_input(&mut self, data: Vec<u8>) {
        let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
        self.route_client_events(events, true);
    }

    #[cfg(test)]
    pub(crate) fn route_client_pixel_mouse(
        &mut self,
        source_id: InputSourceId,
        data: &[u8],
        geometry: crate::input::mouse::HostGeometry,
    ) -> bool {
        let mut presentation = crate::ui::pomodoro::input_presentation_at(
            &self.state,
            ratatui::layout::Rect::new(0, 0, geometry.cols, geometry.rows),
            std::time::Instant::now(),
        );
        self.route_client_pixel_mouse_with_presentation(
            source_id,
            data,
            geometry,
            &mut presentation,
        )
    }

    #[cfg(test)]
    pub(crate) fn route_client_pixel_mouse_with_presentation(
        &mut self,
        source_id: InputSourceId,
        data: &[u8],
        geometry: crate::input::mouse::HostGeometry,
        presentation: &mut crate::ui::pomodoro::InputPresentation,
    ) -> bool {
        let Some((x, y)) = crate::input::mouse::parse_report(data) else {
            return false;
        };
        let Some((column, row)) = geometry.cell(x, y) else {
            return false;
        };
        let Some(cell_report) = crate::input::mouse::report_at_cell(data, column, row) else {
            return false;
        };
        let mut events = crate::raw_input::parse_raw_input_bytes_sync(&cell_report);
        if events.len() != 1 || !matches!(events[0], crate::raw_input::RawInputEvent::Mouse(_)) {
            return false;
        }
        self.state.host_mouse_pixels = Some(crate::input::mouse::HostPixels { x, y, geometry });
        self.route_client_events_from_with_human_input_hook_and_pomodoro_visibility(
            source_id,
            std::mem::take(&mut events),
            false,
            &mut |_| {},
            None,
            presentation,
        );
        self.state.host_mouse_pixels = None;
        true
    }

    #[cfg(test)]
    pub(crate) fn route_client_events(
        &mut self,
        events: Vec<crate::raw_input::RawInputEvent>,
        apply_host_terminal_theme: bool,
    ) {
        self.route_client_events_from(LOCAL_INPUT_SOURCE, events, apply_host_terminal_theme);
    }

    #[cfg(test)]
    pub(crate) fn route_client_events_from(
        &mut self,
        source_id: InputSourceId,
        events: Vec<crate::raw_input::RawInputEvent>,
        apply_host_terminal_theme: bool,
    ) -> bool {
        self.route_client_events_from_with_human_input_hook(
            source_id,
            events,
            apply_host_terminal_theme,
            &mut |_| {},
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn route_client_events_from_with_human_input_hook(
        &mut self,
        source_id: InputSourceId,
        events: Vec<crate::raw_input::RawInputEvent>,
        apply_host_terminal_theme: bool,
        before_terminal_input: &mut impl FnMut(&TerminalInputTarget),
        controlled_owners: Option<&std::collections::HashMap<crate::terminal::TerminalId, u64>>,
    ) -> bool {
        let mut pomodoro_presentation = crate::ui::pomodoro::input_presentation_at(
            &self.state,
            self.state.screen_rect(),
            std::time::Instant::now(),
        );
        // These compatibility entry points predate committed client frames.
        // Preserve their state-based keyboard ownership when a test or local
        // caller has not computed render geometry yet.
        if pomodoro_presentation.prompt.is_none() && self.state.pomodoro.prompt.is_some() {
            pomodoro_presentation.prompt = Some(self.state.screen_rect());
        }
        self.route_client_events_from_with_human_input_hook_and_pomodoro_visibility(
            source_id,
            events,
            apply_host_terminal_theme,
            before_terminal_input,
            controlled_owners,
            &mut pomodoro_presentation,
        )
    }

    pub(crate) fn route_client_events_from_with_human_input_hook_and_pomodoro_visibility(
        &mut self,
        source_id: InputSourceId,
        events: Vec<crate::raw_input::RawInputEvent>,
        apply_host_terminal_theme: bool,
        before_terminal_input: &mut impl FnMut(&TerminalInputTarget),
        controlled_owners: Option<&std::collections::HashMap<crate::terminal::TerminalId, u64>>,
        pomodoro_presentation: &mut crate::ui::pomodoro::InputPresentation,
    ) -> bool {
        self.begin_contract_false_positive_input_burst();
        let mut pomodoro_changed = false;
        for event in events {
            *pomodoro_presentation = crate::ui::pomodoro::input_gate_at(
                &self.state,
                *pomodoro_presentation,
                std::time::Instant::now(),
            );
            let previous_mode = self.state.effective_interaction_mode();
            let owner = self
                .state
                .input_owner_with_pomodoro(pomodoro_presentation.prompt.is_some());
            match event {
                crate::raw_input::RawInputEvent::Key(key) => {
                    self.state.clear_hovered_control();
                    let lease_key = input::InputLeaseKey::new(source_id, &key);
                    let key = self.input_leases.normalize_press(&lease_key, key);
                    let normalized_event = crate::raw_input::RawInputEvent::Key(key.clone());
                    if !self.input_leases.contains(&lease_key)
                        && self.intercept_pomodoro_send_off_raw_input_with_visibility(
                            source_id,
                            &normalized_event,
                            pomodoro_presentation.send_off.is_some(),
                        )
                    {
                        pomodoro_changed = true;
                        if key.kind != crossterm::event::KeyEventKind::Release {
                            self.input_leases.insert_consumed(
                                lease_key,
                                input::ConsumedInputLease::SuppressRepeats,
                            );
                        }
                        continue;
                    }
                    match key.kind {
                        crossterm::event::KeyEventKind::Press => {
                            // A due break reminder still outranks every other
                            // input surface visible beneath it.
                            if owner == state::InputOwner::Pomodoro
                                && self.intercept_notepad_key_with_prompt_visibility(&key, true)
                            {
                                pomodoro_changed = true;
                                self.input_leases.insert_consumed(
                                    lease_key,
                                    input::ConsumedInputLease::SuppressRepeats,
                                );
                                continue;
                            }
                            if self.paste_clipboard_shortcut_for_input_owner(
                                owner,
                                &key.as_key_event(),
                                crate::platform::read_clipboard_text,
                            ) {
                                self.input_leases.insert_consumed(
                                    lease_key,
                                    input::ConsumedInputLease::SuppressRepeats,
                                );
                                continue;
                            }
                            // The headless client path has its own synchronous
                            // router, so it must consume the shared spawn dock
                            // before any pane forwarding can occur.
                            if owner == state::InputOwner::SpawnDock
                                && self.state.spawn_dock.is_some()
                            {
                                self.handle_spawn_dock_key(key.as_key_event());
                                self.input_leases.insert_consumed(
                                    lease_key,
                                    input::ConsumedInputLease::SuppressRepeats,
                                );
                                continue;
                            }
                            if let state::InputOwner::Dock(dock_owner) = owner {
                                if dock_owner != state::DockInputOwner::Editor
                                    && self.handle_dock_key_for_owner_headless(dock_owner, &key)
                                {
                                    self.input_leases.insert_consumed(
                                        lease_key,
                                        input::ConsumedInputLease::SuppressRepeats,
                                    );
                                    continue;
                                }
                            }
                            let terminal_owner = owner.forwards_unhandled_input_to_pane()
                                || matches!(
                                    owner,
                                    state::InputOwner::Popup
                                        | state::InputOwner::Dock(state::DockInputOwner::Editor)
                                );
                            let initial_context = terminal_owner
                                .then(|| self.terminal_input_context())
                                .flatten();
                            let proxy_input_gate_closed =
                                terminal_owner && self.focused_remote_proxy_input_gate_closed();
                            let target = if terminal_owner && initial_context.is_some() {
                                self.handle_terminal_key_headless_from_with_hook(
                                    source_id,
                                    key.clone(),
                                    before_terminal_input,
                                    controlled_owners,
                                )
                            } else {
                                self.handle_non_terminal_key_headless(owner, key.clone());
                                None
                            };
                            let resulting_context = terminal_owner
                                .then(|| self.terminal_input_context())
                                .flatten();
                            let plan = self.input_leases.complete_press_with_reprocess(
                                lease_key,
                                &key,
                                initial_context.as_ref(),
                                resulting_context.as_ref(),
                                target,
                                !proxy_input_gate_closed,
                            );
                            self.execute_repeat_plan_headless(
                                source_id,
                                lease_key,
                                key,
                                plan,
                                before_terminal_input,
                                controlled_owners,
                            );
                        }
                        crossterm::event::KeyEventKind::Repeat => {
                            let current_context = (owner.forwards_unhandled_input_to_pane()
                                || matches!(
                                    owner,
                                    state::InputOwner::Popup
                                        | state::InputOwner::Dock(state::DockInputOwner::Editor)
                                ))
                            .then(|| self.terminal_input_context())
                            .flatten();
                            let plan = self.input_leases.plan_repeat(
                                lease_key,
                                &key,
                                current_context.as_ref(),
                            );
                            self.execute_repeat_plan_headless(
                                source_id,
                                lease_key,
                                key,
                                plan,
                                before_terminal_input,
                                controlled_owners,
                            );
                        }
                        crossterm::event::KeyEventKind::Release => {
                            if let Some(lease) = self.input_leases.remove_forwarded(&lease_key) {
                                let release = key.with_windows_composition_from(&lease.key);
                                let _ = self.forward_terminal_key_to_target_headless(
                                    &lease.target,
                                    release,
                                );
                            }
                        }
                    }
                }
                _ if self.intercept_pomodoro_send_off_raw_input_with_visibility(
                    source_id,
                    &event,
                    pomodoro_presentation.send_off.is_some(),
                ) =>
                {
                    pomodoro_changed = true;
                    continue;
                }
                crate::raw_input::RawInputEvent::Text(text) => {
                    if pomodoro_presentation.prompt.is_some() {
                        pomodoro_changed = true;
                        continue;
                    }
                    self.state.clear_hovered_control();
                    self.handle_text_commit_headless_for_owner_with_hook(
                        owner,
                        text.as_str(),
                        before_terminal_input,
                        controlled_owners,
                    );
                }
                crate::raw_input::RawInputEvent::Mouse(mouse) => {
                    if owner == state::InputOwner::Pomodoro {
                        pomodoro_changed = true;
                    }
                    self.handle_mouse_for_input_owner(
                        source_id,
                        mouse,
                        *pomodoro_presentation,
                        owner,
                    );
                }
                crate::raw_input::RawInputEvent::Paste(text) => {
                    if owner == state::InputOwner::Pomodoro {
                        pomodoro_changed = true;
                        continue;
                    }
                    self.state.clear_hovered_control();
                    if owner.forwards_unhandled_input_to_pane()
                        && self
                            .state
                            .active
                            .and_then(|ws_idx| {
                                self.state
                                    .workspaces
                                    .get(ws_idx)
                                    .and_then(|ws| ws.focused_pane_id())
                            })
                            .is_some_and(|pane_id| {
                                self.state
                                    .settled_view
                                    .as_ref()
                                    .is_some_and(|view| view.pane_id == pane_id)
                            })
                    {
                        continue;
                    }
                    if owner == state::InputOwner::Popup {
                        self.try_route_paste_to_popup(&text);
                    } else if owner == state::InputOwner::Dock(state::DockInputOwner::Editor) {
                        if let Some(runtime) = self.dock_editor_runtime() {
                            let _ = runtime.try_send_paste(text);
                        }
                    } else if self.paste_into_input_owner(owner, &text) {
                    } else if owner.forwards_unhandled_input_to_pane() {
                        if let Some(ws_idx) = self.state.active {
                            let focused = self
                                .state
                                .workspaces
                                .get(ws_idx)
                                .and_then(|workspace| workspace.focused_pane_id());
                            let has_text = !text.is_empty();
                            if has_text {
                                if let Some(focused) = focused {
                                    self.resume_settled_pane_before_input(focused);
                                }
                            }
                            if let Some(ws) = self.state.workspaces.get(ws_idx) {
                                if let Some(focused) = focused {
                                    if let Some(runtime) = self.state.runtime_for_pane_in_workspace(
                                        &self.terminal_runtimes,
                                        ws_idx,
                                        focused,
                                    ) {
                                        if has_text {
                                            if let Some(terminal_id) =
                                                ws.terminal_id(focused).cloned()
                                            {
                                                #[cfg(unix)]
                                                if let Some(owner_id) = controlled_owners
                                                    .and_then(|owners| owners.get(&terminal_id))
                                                {
                                                    runtime.release_remote_owner(*owner_id);
                                                }
                                                before_terminal_input(&TerminalInputTarget::new(
                                                    terminal_id,
                                                ));
                                            }
                                        }
                                        let sent = runtime.try_send_paste(text).is_ok();
                                        if sent && has_text {
                                            self.retire_blocked_hook_authority_for_pane(
                                                focused,
                                                std::time::Instant::now(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                crate::raw_input::RawInputEvent::OuterFocusGained => {
                    if self.state.symphony_detail.is_none() && self.state.work_view.is_none() {
                        self.send_outer_focus_event(crate::ghostty::FocusEvent::Gained);
                    }
                }
                crate::raw_input::RawInputEvent::OuterFocusLost => {
                    self.release_input_source_headless(source_id);
                    if self.state.symphony_detail.is_none() && self.state.work_view.is_none() {
                        self.send_outer_focus_event(crate::ghostty::FocusEvent::Lost);
                    }
                }
                crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                    if apply_host_terminal_theme {
                        self.update_host_terminal_theme(kind, color);
                    }
                }
                crate::raw_input::RawInputEvent::HostPaletteColors { colors } => {
                    if apply_host_terminal_theme {
                        self.update_host_terminal_palette_colors(&colors);
                    }
                }
                crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                    if apply_host_terminal_theme {
                        self.set_host_terminal_appearance(appearance, true);
                    }
                }
                crate::raw_input::RawInputEvent::HostCellSizeReport { .. } => {}
                crate::raw_input::RawInputEvent::Unsupported => {}
            }
            self.sync_prefix_input_source(previous_mode);
        }
        *pomodoro_presentation = crate::ui::pomodoro::input_gate_at(
            &self.state,
            *pomodoro_presentation,
            std::time::Instant::now(),
        );
        pomodoro_changed
    }

    pub(crate) fn clear_input_source(&mut self, source_id: InputSourceId) {
        // Call this only when the input source is gone for good. A pending URL
        // click has to outlive a plain focus change, because opening the URL
        // raises the browser and costs the host terminal its focus before the
        // mouse release arrives.
        self.pending_url_click_sources.remove(&source_id);
        self.pending_config_diagnostic_click_sources
            .remove(&source_id);
        self.release_input_source_headless(source_id);
    }

    /// Handles a key event in non-terminal mode for the headless server.
    ///
    /// Uses the standalone handler functions that work on `&mut AppState`
    /// since the server doesn't have the async context of the monolithic App.
    fn handle_non_terminal_key_headless(
        &mut self,
        owner: state::InputOwner,
        key: crate::input::TerminalKey,
    ) {
        let key_event = key.as_key_event();
        if self.handle_sidebar_areas_menu_key(key_event, owner) {
            return;
        }
        match owner {
            state::InputOwner::Pomodoro | state::InputOwner::Popup | state::InputOwner::Pane => {}
            state::InputOwner::SpawnDock => self.handle_spawn_dock_key(key_event),
            state::InputOwner::Client(owner) => match owner {
                state::ClientInputOwner::Overlay(overlay) => {
                    self.handle_client_overlay_key(overlay, key_event);
                }
                state::ClientInputOwner::SnoozeMenu => {
                    self.handle_sidebar_snooze_menu_key(key_event);
                }
                state::ClientInputOwner::SnoozeTime => {
                    self.handle_sidebar_snooze_time_key(key_event);
                }
                state::ClientInputOwner::SettledMenu
                | state::ClientInputOwner::SettledDeleteConfirm => {
                    self.handle_sidebar_settled_key(key_event);
                }
                state::ClientInputOwner::AgentPicker => self.handle_agent_picker_key(key_event),
                state::ClientInputOwner::SidebarGroupMenu => {
                    self.state.handle_sidebar_group_menu_key(key_event);
                }
                state::ClientInputOwner::SidebarFilterMenu => {
                    self.state.handle_sidebar_filter_menu_key(key_event);
                }
                state::ClientInputOwner::SidebarNewMenu => {
                    self.state.handle_sidebar_new_menu_key(key_event);
                }
                state::ClientInputOwner::SidebarNewThread => {
                    self.state.handle_sidebar_new_thread_key(key_event);
                }
                state::ClientInputOwner::SidebarProjectMenu => {
                    self.state.handle_sidebar_project_menu_key(key_event);
                }
                state::ClientInputOwner::SidebarObjectMenu => {
                    self.handle_sidebar_object_menu_key(key_event);
                }
                state::ClientInputOwner::SidebarSortMenu => {
                    self.state.handle_sidebar_sort_menu_key(key_event);
                }
                state::ClientInputOwner::SidebarSubgroupPicker => {
                    self.state.handle_sidebar_subgroup_picker_key(key_event);
                }
                state::ClientInputOwner::SidebarPodPicker => {
                    self.handle_sidebar_pod_picker_key(key_event);
                }
                state::ClientInputOwner::PrActionConfirmation => {
                    self.handle_pr_action_confirmation_key(key_event);
                }
                state::ClientInputOwner::DockSurfaceMenu => {
                    self.handle_dock_surface_menu_key(&key);
                }
            },
            state::InputOwner::AddProject
            | state::InputOwner::Surface(state::SurfaceInputOwner::Home) => {
                self.handle_home_key_headless(key_event);
            }
            state::InputOwner::Server(owner) => match owner {
                state::ServerInputOwner::Onboarding => self.handle_onboarding_key(key_event),
                state::ServerInputOwner::ReleaseNotes => self.handle_release_notes_key(key_event),
                state::ServerInputOwner::ProductAnnouncement => {
                    self.handle_product_announcement_key(key_event)
                }
                state::ServerInputOwner::Navigate => self.handle_navigate_key(key),
                state::ServerInputOwner::Prefix => self.handle_prefix_key(key),
                state::ServerInputOwner::Copy => self.handle_copy_mode_key(key),
                state::ServerInputOwner::Resize => self.handle_resize_key_via_api(key),
                state::ServerInputOwner::GitMenu => {
                    input::handle_git_menu_key(&mut self.state, key_event)
                }
                state::ServerInputOwner::AddAction => self.handle_add_action_key(key_event),
                state::ServerInputOwner::Settings => self.handle_settings_key(key_event),
                state::ServerInputOwner::GlobalMenu => {
                    input::handle_global_menu_key(&mut self.state, key_event)
                }
                state::ServerInputOwner::KeybindHelp => {
                    input::handle_keybind_help_key(&mut self.state, key)
                }
                state::ServerInputOwner::Navigator => {
                    input::handle_navigator_key(&mut self.state, &self.terminal_runtimes, key_event)
                }
                state::ServerInputOwner::CommandPalette => {
                    self.handle_command_palette_key(key_event)
                }
                state::ServerInputOwner::WorkLinkPicker => {
                    self.handle_work_link_picker_key(key_event)
                }
            },
            state::InputOwner::Surface(state::SurfaceInputOwner::Symphony) => {
                self.handle_symphony_key(key_event);
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::LoopRunHistory) => {
                self.handle_loop_run_history_key(key_event);
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::AloopRunLog) => {
                self.handle_aloop_run_detail_key(key_event);
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::Usage) => {
                self.handle_usage_view_key(key_event);
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::Work) => {
                self.handle_work_view_key(key_event);
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::Board) => {
                self.handle_board_key(key_event);
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::DockObjectPreview) => {
                if key_event.code == crossterm::event::KeyCode::Esc
                    && key_event.modifiers.is_empty()
                {
                    self.state.dock_object_preview = None;
                    self.state.dock_pr_focused = false;
                    self.state.dock_linear_focused = false;
                }
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::Inbox) => {
                self.handle_inbox_key_headless(key_event);
            }
            state::InputOwner::Surface(state::SurfaceInputOwner::EditorPreview) => {}
            state::InputOwner::Notepad => {
                self.intercept_notepad_key_with_prompt_visibility(&key, false);
            }
            state::InputOwner::Dock(owner) => {
                self.handle_dock_key_for_owner_headless(owner, &key);
            }
            state::InputOwner::Sidebar => {
                if self.state.handle_sidebar_search_key(key_event) {
                    return;
                }
                if self.handle_sidebar_object_menu_key(key_event) {
                    return;
                }
                match self.state.handle_sidebar_work_group_key(key_event) {
                    input::SidebarWorkGroupKeyAction::Ignored => {}
                    input::SidebarWorkGroupKeyAction::Consumed => return,
                    input::SidebarWorkGroupKeyAction::Dispatch(plan) => {
                        self.dispatch_sidebar_work_group_plan(*plan);
                        return;
                    }
                    input::SidebarWorkGroupKeyAction::RenamePod(record) => {
                        input::open_rename_pod(&mut self.state, record);
                        return;
                    }
                    input::SidebarWorkGroupKeyAction::DeletePod(record) => {
                        self.runtime_delete_pod(record);
                        return;
                    }
                }
                if !self.handle_sidebar_session_action_key(key_event) {
                    self.handle_sidebar_settled_key(key_event);
                }
            }
            state::InputOwner::None => {}
        }
    }

    fn handle_dock_key_for_owner_headless(
        &mut self,
        owner: state::DockInputOwner,
        key: &crate::input::TerminalKey,
    ) -> bool {
        if self.handle_dock_chooser_key_headless(key) {
            return true;
        }
        match owner {
            state::DockInputOwner::Home => self.handle_dock_home_key_headless(key),
            state::DockInputOwner::PullRequest => self.handle_dock_pr_key_headless(key),
            state::DockInputOwner::Linear => self.handle_dock_linear_key_headless(key),
            state::DockInputOwner::Diff => self.handle_dock_diff_key_headless(key),
            state::DockInputOwner::Files => self.handle_dock_files_key(key),
            state::DockInputOwner::Agents => self.handle_dock_agents_key(key),
            state::DockInputOwner::Hosts => self.handle_dock_hosts_key(key),
            state::DockInputOwner::Editor | state::DockInputOwner::Chooser => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::terminal::TerminalRuntime;
    use crate::workspace::Workspace;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn saved_window_cycle_mode_wins_over_restored_session_snapshot() {
        let saved_config = "[ui]\nwindow_cycle_mode = \"this-machine-and-fleet\"\n";
        let restored_config: Config = toml::from_str(saved_config).expect("restore config");

        let stale_snapshot = crate::persist::capture(
            &[],
            &std::collections::HashMap::new(),
            &crate::terminal::TerminalRuntimeRegistry::new(),
            None,
            0,
            24,
            0.5,
            std::collections::HashSet::new(),
            false,
            crate::config::WindowCycleModeConfig::ThisMachine,
            false,
        );

        assert_eq!(
            window_cycle_mode_after_restore(
                restored_config.ui.window_cycle_mode,
                stale_snapshot.window_cycle_mode,
            ),
            crate::config::WindowCycleModeConfig::ThisMachineAndFleet
        );
    }

    #[test]
    fn fresh_and_restored_clients_start_with_no_panel_tabs() {
        let app = App::new(
            &Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        assert!(app.state.dock_default_surfaces.is_empty());
        assert!(app.state.dock_open_surfaces.is_empty());
        assert_eq!(app.state.dock_tab, None);

        let restored_attach = state::DockPresentationState::default();
        assert!(restored_attach.open_surfaces.is_empty());
        assert_eq!(restored_attach.tab, None);
    }

    #[test]
    fn panel_config_restores_only_the_selected_default_tabs() {
        let mut config = Config::default();
        config.panel.default_surfaces = vec![
            crate::config::PanelSurfaceConfig::Files,
            crate::config::PanelSurfaceConfig::PullRequest,
            crate::config::PanelSurfaceConfig::Files,
        ];
        let app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );

        assert_eq!(
            app.state.dock_default_surfaces,
            vec![state::DockSurface::Files, state::DockSurface::Pr]
        );
        assert_eq!(
            app.state.dock_open_surfaces,
            app.state.dock_default_surfaces
        );
        assert_eq!(app.state.dock_tab, Some(state::DockSurface::Files));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn handoff_restoration_warms_local_identity_cache() {
        let mut source = state::AppState::test_new();
        source.workspaces = vec![Workspace::test_new("handoff")];
        source.ensure_test_terminals();
        source.active = Some(0);
        source.selected = 0;
        let snapshot = crate::persist::capture(
            &source.workspaces,
            &source.terminals,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            source.active,
            source.selected,
            source.sidebar_width,
            source.sidebar_section_split,
            source.collapsed_space_keys.clone(),
            source.prio_panel_collapsed,
            source.window_cycle_mode,
            source.skip_collapsed_cycle,
        );
        let mut imports = std::collections::HashMap::new();

        let app = App::new_from_handoff(
            &Config::default(),
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
            &snapshot,
            &mut imports,
            &[],
        )
        .expect("restore handoff");

        let workspace = &app.state.workspaces[0];
        let pane_id = workspace.tabs[0].root_pane;
        let cached = app
            .state
            .local_agent_panel_identities
            .get(&pane_id)
            .expect("warm local identity cache");
        assert_eq!(cached.workspace_id, workspace.id);
        assert_eq!(cached.local_target.ws_idx, 0);
        assert_eq!(cached.local_target.tab_idx, 0);
        assert_eq!(cached.local_target.pane_id, pane_id);
    }

    #[cfg(unix)]
    fn imported_editor_runtime(
        pane_id: crate::layout::PaneId,
    ) -> (
        crate::terminal::TerminalRuntime,
        crate::handoff_runtime::ImportedHandoffRuntime,
    ) {
        let (events, _events_rx) = tokio::sync::mpsc::channel(16);
        let runtime = TerminalRuntime::spawn_argv_command(
            pane_id,
            24,
            80,
            std::env::temp_dir(),
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ],
            &crate::pane::PaneLaunchEnv::default(),
            crate::pane::AgentDetection::Disabled,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .expect("spawn editor pane");
        runtime
            .pause_handoff_reader(std::time::Duration::from_secs(2))
            .expect("quiesce the pty actor");
        let master_fd = runtime.duplicate_handoff_fd().expect("duplicate pty fd");
        let state = runtime.handoff_runtime_state(pane_id.raw());
        (
            runtime,
            crate::handoff_runtime::ImportedHandoffRuntime {
                master_fd: Some(master_fd),
                state,
            },
        )
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_imported_dock_editor_reattaches_to_its_renamed_agent_pane() {
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("editor")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let agent_pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let exported_agent_pane_id = agent_pane_id.raw() + 1_000;
        app.state
            .pane_id_aliases
            .insert(exported_agent_pane_id, agent_pane_id);
        let exported_editor_pane_id = crate::layout::PaneId::alloc();
        let (source, import) = imported_editor_runtime(exported_editor_pane_id);
        let child_pid = source.child_pid().expect("editor child pid");

        app.restore_handoff_dock_editors(
            &Config::default(),
            vec![(
                crate::server::handoff::DockEditorHandoff {
                    agent_pane_id: exported_agent_pane_id,
                    editor_pane_id: exported_editor_pane_id.raw(),
                },
                import,
            )],
        );

        let session = app
            .state
            .dock_editor_sessions
            .get(&agent_pane_id)
            .expect("editor reattached to its agent pane")
            .clone();
        assert!(app.terminal_runtimes.get(&session.terminal_id).is_some());
        assert_ne!(session.pane_id, exported_editor_pane_id);
        assert_eq!(unsafe { libc::kill(child_pid as i32, 0) }, 0);
        drop(source);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_imported_dock_editor_is_dropped_when_its_agent_pane_is_gone() {
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let exported_editor_pane_id = crate::layout::PaneId::alloc();
        let (source, import) = imported_editor_runtime(exported_editor_pane_id);

        app.restore_handoff_dock_editors(
            &Config::default(),
            vec![(
                crate::server::handoff::DockEditorHandoff {
                    agent_pane_id: 4_242,
                    editor_pane_id: exported_editor_pane_id.raw(),
                },
                import,
            )],
        );

        assert!(app.state.dock_editor_sessions.is_empty());
        drop(source);
    }

    fn raw_key(
        code: KeyCode,
        modifiers: KeyModifiers,
        kind: KeyEventKind,
    ) -> crate::raw_input::RawInputEvent {
        crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(code, modifiers).with_kind(kind),
        )
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn native_repeats_and_releases_follow_the_pressed_pane() {
        let record = crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 27,
            virtual_scan_code: 1,
            unicode: 27,
            control_key_state: 0,
        };
        let key = crate::input::TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())
            .with_windows_record(record);
        let enhanced = crate::input::TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())
            .with_windows_record(crate::input::WindowsKeyRecord {
                control_key_state: 0x0100,
                ..record
            });
        assert_ne!(
            input::InputLeaseKey::new(7, &key),
            input::InputLeaseKey::new(7, &enhanced)
        );

        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let pressed_pane = workspace.focused_pane_id().unwrap();
        let other_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(pressed_pane);
        let (pressed_runtime, mut pressed_rx) =
            TerminalRuntime::test_with_channel_capacity(80, 24, 6);
        let (other_runtime, mut other_rx) = TerminalRuntime::test_with_channel_capacity(80, 24, 6);
        workspace.tabs[0]
            .runtimes
            .insert(pressed_pane, pressed_runtime);
        workspace.tabs[0].runtimes.insert(other_pane, other_runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let release = crate::input::TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())
            .with_windows_record(crate::input::WindowsKeyRecord {
                key_down: false,
                repeat_count: 1,
                unicode: 0,
                ..record
            })
            .with_kind(KeyEventKind::Release);
        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(key.clone())],
            false,
        );
        assert!(app.state.focus_pane_in_workspace(0, other_pane));
        app.route_client_events(
            vec![
                crate::raw_input::RawInputEvent::Key(key.clone()),
                crate::raw_input::RawInputEvent::Key(key.clone().with_kind(KeyEventKind::Repeat)),
                crate::raw_input::RawInputEvent::Key(release),
                crate::raw_input::RawInputEvent::Key(key.clone()),
                crate::raw_input::RawInputEvent::OuterFocusLost,
            ],
            false,
        );

        for _ in 0..3 {
            assert_eq!(
                pressed_rx.try_recv().unwrap(),
                bytes::Bytes::from_static(b"\x1b[27;1;27;1;0;1_")
            );
        }
        assert_eq!(
            pressed_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[27;1;0;0;0;1_")
        );
        assert!(pressed_rx.try_recv().is_err());

        assert_eq!(
            other_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[27;1;27;1;0;1_")
        );
        assert_eq!(
            other_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[27;1;27;0;0;1_")
        );
        assert!(other_rx.try_recv().is_err());
        assert!(app.input_leases.is_empty());
    }

    fn release_notes_state() -> state::ReleaseNotesState {
        state::ReleaseNotesState {
            version: "0.1.0".into(),
            body: "notes".into(),
            scroll: 0,
            preview: true,
        }
    }

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = Config::default();
        config.work_index.enabled = false;
        App::new(&config, true, None, api_rx, crate::api::EventHub::default())
    }

    #[test]
    fn diagnostic_marker_hover_tracks_pointer_and_click_dismisses() {
        let mut app = test_app();
        app.state.config_diagnostic = Some("config warning".into());
        app.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(5));
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 80, 24));
        let marker = app.state.view.config_diagnostic_hit_area;

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: marker.x,
            row: marker.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            app.state.hovered_control,
            Some(state::ControlId::ConfigDiagnostic)
        );

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: marker.x.saturating_sub(1),
            row: marker.y,
            modifiers: KeyModifiers::NONE,
        });
        // The cell beside the marker is a status segment with its own tooltip.
        assert_ne!(
            app.state.hovered_control,
            Some(state::ControlId::ConfigDiagnostic)
        );

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: marker.x,
            row: marker.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.state.config_diagnostic.is_none());
        assert!(app.config_diagnostic_deadline.is_none());

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: marker.x,
            row: marker.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.state.config_diagnostic.is_none());
        assert!(app.pending_config_diagnostic_click_sources.is_empty());
    }

    #[cfg(unix)]
    fn test_app_with_focused_runtime() -> (
        App,
        crate::terminal::TerminalId,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("remote-control-input");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .expect("focused terminal")
            .clone();
        let (runtime, input_rx) = TerminalRuntime::test_with_channel_capacity(80, 24, 4);
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        (app, terminal_id, input_rx)
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn only_human_input_routed_to_controlled_pane_ends_remote_lease() {
        let (mut app, terminal_id, mut input_rx) = test_app_with_focused_runtime();
        let owner_id = 41;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        {
            let runtime = app
                .state
                .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
                .expect("focused runtime");
            assert!(runtime.acquire_remote_owner(owner_id));
        }
        let controlled_owners = std::collections::HashMap::from([(terminal_id.clone(), owner_id)]);

        app.route_client_events_from_with_human_input_hook(
            9,
            vec![
                crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: 2,
                    row: 2,
                    modifiers: KeyModifiers::empty(),
                }),
                crate::raw_input::RawInputEvent::OuterFocusGained,
            ],
            false,
            &mut |_| {},
            Some(&controlled_owners),
        );
        assert!(!app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("focused runtime")
            .acquire_remote_owner(99));

        app.route_client_events_from_with_human_input_hook(
            9,
            vec![raw_key(
                KeyCode::Char('x'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
            &mut |_| {},
            Some(&controlled_owners),
        );
        assert!(app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("focused runtime")
            .acquire_remote_owner(owner_id));
        assert!(input_rx.try_recv().is_ok());

        app.route_client_events_from_with_human_input_hook(
            9,
            vec![crate::raw_input::RawInputEvent::Paste(
                "clipboard-image".into(),
            )],
            false,
            &mut |_| {},
            Some(&controlled_owners),
        );
        assert!(app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("focused runtime")
            .acquire_remote_owner(owner_id));
        assert!(input_rx.try_recv().is_ok());
    }

    #[test]
    fn pending_first_frame_marker_waits_for_visible_pane() {
        let mut app = test_app();
        let pane_id = crate::layout::PaneId::alloc();
        app.pending_first_frame_pane = Some(pane_id);

        app.record_pending_first_frame();
        assert_eq!(app.pending_first_frame_pane, Some(pane_id));

        app.state.view.pane_infos.push(crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::NONE,
            is_focused: true,
        });
        app.record_pending_first_frame();
        assert!(app.pending_first_frame_pane.is_none());
    }

    #[test]
    fn stale_work_index_generation_is_rejected() {
        let mut app = test_app();
        app.last_work_index_refresh_generation = 4;
        app.last_applied_work_index_refresh_generation = 4;
        let snapshot = crate::work_index::Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::now(),
        };
        assert!(!app.handle_work_index_refreshed(
            3,
            snapshot,
            crate::work_index::WorkIndexSession::default(),
        ));
        assert!(app.work_index_snapshot.is_none());
    }

    #[test]
    fn disabled_work_index_does_not_start_a_refresh() {
        let mut app = test_app();
        app.work_index_config.enabled = false;
        app.start_work_index_refresh_if_due(Instant::now());
        assert!(app.work_index_refresh_in_flight.is_none());
    }

    #[test]
    fn opening_home_on_launch_is_a_no_op_unless_the_config_asks_for_it() {
        let mut app = test_app();
        assert!(app.state.home.is_none(), "the constructor opens nothing");

        app.state.open_home_on_launch(&Config::default());
        assert!(
            app.state.home.is_none(),
            "home is opt-in: the launch screen hides the panes you asked for"
        );

        let mut config = Config::default();
        config.ui.show_home_on_start = true;
        app.state.open_home_on_launch(&config);
        assert!(app.state.home.is_some());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn stale_full_lifecycle_hook_authority_falls_back_to_screen_in_tui_scheduler() {
        for (fallback, visible_blocker, visible_working, screen) in [
            (
                crate::detect::AgentState::Blocked,
                true,
                false,
                b"\xe2\x96\xb3 Permission required\n".as_slice(),
            ),
            (
                crate::detect::AgentState::Working,
                false,
                true,
                b"esc interrupt\n".as_slice(),
            ),
        ] {
            let mut app = test_app();
            app.state.workspaces = vec![Workspace::test_new("hook-expiry")];
            app.state.ensure_test_terminals();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let reported_at = Instant::now();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state_with_screen_signals_at(
                Some(crate::detect::Agent::Kilo),
                fallback,
                visible_blocker,
                false,
                visible_working,
                false,
                false,
                reported_at,
            );
            let session_ref = crate::agent_resume::AgentSessionRef::id("tui-expiry").unwrap();
            terminal.set_agent_session_ref_for_session_start(
                "herdr:kilo".into(),
                "kilo".into(),
                Some(session_ref.clone()),
                Some(1),
                Some("startup".into()),
            );
            terminal.set_hook_authority_at(
                "herdr:kilo".into(),
                "kilo".into(),
                crate::detect::AgentState::Working,
                None,
                Some(session_ref),
                Some(2),
                reported_at,
            );
            app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane_id)
                .unwrap()
                .seen = false;
            let (runtime, mut detection_events) =
                crate::terminal::TerminalRuntime::test_with_live_detection_screen_bytes(
                    pane_id,
                    crate::detect::Agent::Kilo,
                    fallback,
                    false,
                    visible_blocker,
                    visible_working,
                    screen,
                );
            app.terminal_runtimes.insert(terminal_id.clone(), runtime);
            app.sync_detection_authority_mirrors();
            let deadline = app
                .state
                .next_full_lifecycle_hook_authority_deadline()
                .unwrap();

            assert!(app.handle_scheduled_tasks(deadline, false));
            let event = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let event = detection_events.recv().await.expect("detector event");
                    if matches!(event, AppEvent::StateChanged { .. }) {
                        break event;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("expiry must rescan cached {fallback:?} evidence"));
            let AppEvent::StateChanged { state, .. } = &event else {
                unreachable!()
            };
            assert_eq!(*state, fallback, "expiry must not synthesize Idle");
            app.handle_internal_event(event);
            assert_eq!(
                app.state.terminals[&terminal_id].raw_agent_state(),
                fallback
            );
            assert_ne!(
                app.agent_info(0, pane_id).unwrap().agent_status,
                crate::api::schema::AgentStatus::Done,
                "hidden pane must not flicker Done during detector rescan"
            );
            assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
        }
    }

    #[test]
    fn stale_full_lifecycle_hook_authority_falls_back_to_screen_invalid_reload_keeps_value() {
        let mut app = test_app();
        let mut config = Config::default();
        config
            .agent_detection
            .full_lifecycle_hook_authority_timeout_seconds = 30;
        app.apply_live_config(&config, &[], &[], false);
        assert_eq!(
            app.state.full_lifecycle_hook_authority_timeout,
            Duration::from_secs(30)
        );

        config
            .agent_detection
            .full_lifecycle_hook_authority_timeout_seconds = 600;
        app.apply_live_config(&config, &[], &["agent_detection".into()], false);
        assert_eq!(
            app.state.full_lifecycle_hook_authority_timeout,
            Duration::from_secs(30)
        );
    }

    fn focus_reporting_app() -> (App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("focus-reporting");
        let pane_id = workspace.tabs[0].root_pane;
        let (runtime, input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[?1004h",
                4,
            );
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        (app, input_rx)
    }

    fn unique_temp_path(name: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("herdr-{name}-{}-{stamp}", std::process::id()))
    }

    #[cfg(windows)]
    fn exiting_test_command() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn exiting_test_command() -> &'static str {
        "/usr/bin/true"
    }

    #[derive(Clone, Default)]
    struct FakePrefixInputSource {
        switch_calls: Rc<Cell<usize>>,
        restore_calls: Rc<Cell<usize>>,
        switched: Rc<Cell<bool>>,
        will_switch: bool,
    }

    impl FakePrefixInputSource {
        fn switching() -> Self {
            Self {
                will_switch: true,
                ..Self::default()
            }
        }

        fn no_op() -> Self {
            Self {
                will_switch: false,
                ..Self::default()
            }
        }
    }

    impl crate::platform::PrefixInputSource for FakePrefixInputSource {
        fn switch_to_ascii(&mut self) {
            self.switch_calls.set(self.switch_calls.get() + 1);
            if self.will_switch {
                self.switched.set(true);
            }
        }

        fn restore(&mut self) {
            if self.switched.replace(false) {
                self.restore_calls.set(self.restore_calls.get() + 1);
            }
        }
    }

    /// Drain the app event channel, returning the `active` flags of any emitted
    /// `PrefixInputSource` events (the host-local input-source intents).
    fn drained_prefix_active(app: &mut App) -> Vec<bool> {
        let mut out = Vec::new();
        while let Ok(ev) = app.event_rx.try_recv() {
            if let crate::events::AppEvent::PrefixInputSource { active, .. } = ev {
                out.push(active);
            }
        }
        out
    }

    #[test]
    fn sync_prefix_input_source_emits_switch_then_restore_when_enabled() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;

        // Terminal -> Prefix emits the ASCII-switch intent.
        app.state.set_server_mode(Mode::Prefix);
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Prefix -> Terminal emits the restore intent.
        app.state.set_server_mode(Mode::Terminal);
        app.sync_prefix_input_source(Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn sync_prefix_input_source_does_not_emit_switch_when_flag_disabled() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = false;

        // Entering the realm with the flag off emits nothing.
        app.state.set_server_mode(Mode::Prefix);
        app.sync_prefix_input_source(Mode::Terminal);
        assert!(drained_prefix_active(&mut app).is_empty());

        // Leaving the realm still emits the restore (harmless if nothing was switched), so a
        // mid-interaction flag toggle can't strand the host on ASCII.
        app.state.set_server_mode(Mode::Terminal);
        app.sync_prefix_input_source(Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn mode_wants_ascii_input_classification() {
        // Allowlist: the prefix command/navigation realm wants ASCII.
        for mode in [
            Mode::Prefix,
            Mode::Navigate,
            Mode::Navigator,
            Mode::CommandPalette,
            Mode::Copy,
            Mode::Resize,
            Mode::ConfirmClose,
            Mode::ConfirmRemoveWorktree,
            Mode::ContextMenu,
            Mode::GlobalMenu,
            Mode::KeybindHelp,
        ] {
            assert!(mode.wants_ascii_input(), "{mode:?} should want ASCII");
        }
        // Everything else (terminal, text entry, startup overlays) keeps the user's IME.
        for mode in [
            Mode::Terminal,
            Mode::RenameWorkspace,
            Mode::RenameTab,
            Mode::RenamePane,
            Mode::NewLinkedWorktree,
            Mode::OpenExistingWorktree,
            Mode::Settings,
            Mode::Onboarding,
            Mode::ReleaseNotes,
            Mode::ProductAnnouncement,
        ] {
            assert!(!mode.wants_ascii_input(), "{mode:?} should keep the IME");
        }
    }

    #[test]
    fn sync_prefix_input_source_keeps_realm_across_multi_level_prefix_commands() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;

        // Terminal -> Prefix switches once.
        app.state.set_server_mode(Mode::Prefix);
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Prefix -> sub-mode and sub-mode -> sub-mode stay in the realm: no emit.
        app.state.set_server_mode(Mode::Navigator);
        app.sync_prefix_input_source(Mode::Prefix);
        app.state.set_server_mode(Mode::Resize);
        app.sync_prefix_input_source(Mode::Navigator);
        assert!(
            drained_prefix_active(&mut app).is_empty(),
            "must not switch or restore while still in the realm"
        );

        // Leaving the realm back to the terminal restores.
        app.state.set_server_mode(Mode::Terminal);
        app.sync_prefix_input_source(Mode::Resize);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn sync_prefix_input_source_restores_when_entering_rename_text_mode() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;

        app.state.set_server_mode(Mode::Prefix);
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Prefix -> RenameTab leaves the realm (text entry wants the IME): restore.
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        app.sync_prefix_input_source(Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn handle_internal_event_prefix_input_source_applies_switch_and_restore() {
        // The monolithic (in-process) path applies the host switch when it consumes the event.
        let mut app = test_app();
        let fake = FakePrefixInputSource::switching();
        let switch_calls = fake.switch_calls.clone();
        let restore_calls = fake.restore_calls.clone();
        app.set_prefix_input_source(Box::new(fake));

        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource {
            client_id: None,
            active: true,
        });
        assert_eq!(switch_calls.get(), 1);
        assert_eq!(restore_calls.get(), 0);

        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource {
            client_id: None,
            active: false,
        });
        assert_eq!(restore_calls.get(), 1);
    }

    #[test]
    fn handle_internal_event_prefix_input_source_restore_is_safe_when_switch_was_noop() {
        // Already-ASCII / failed-switch case: the restore on leave must stay harmless.
        let mut app = test_app();
        let fake = FakePrefixInputSource::no_op();
        let switch_calls = fake.switch_calls.clone();
        let restore_calls = fake.restore_calls.clone();
        app.set_prefix_input_source(Box::new(fake));

        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource {
            client_id: None,
            active: true,
        });
        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource {
            client_id: None,
            active: false,
        });
        assert_eq!(switch_calls.get(), 1);
        assert_eq!(restore_calls.get(), 0);
    }

    #[tokio::test]
    async fn raw_input_dispatch_emits_input_source_intent_when_leaving_prefix() {
        // Leaving prefix mode happens inside the raw-input dispatch, not in `handle_key` itself —
        // the sync must sit at the dispatch layer so any event that exits prefix (here Esc) still
        // emits the restore intent.
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        // ctrl+b (the default prefix key) enters prefix mode → switch intent.
        app.handle_raw_input_event(raw_key(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ))
        .await;
        assert_eq!(app.state.server_mode(), Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Esc leaves prefix mode → restore intent.
        app.handle_raw_input_event(raw_key(
            KeyCode::Esc,
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ))
        .await;
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[tokio::test]
    async fn context_menu_input_source_transition_restores_on_escape() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.context_menu = Some(state::ContextMenuState {
            kind: state::ContextMenuKind::Workspace {
                workspace_id: app.state.workspaces[0].id.clone(),
                ws_idx: 0,
            },
            x: 2,
            y: 2,
            selected: state::ContextMenuAction::RenameWorkspace,
        });

        app.state
            .open_client_overlay(state::ClientOverlay::ContextMenu);
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        app.handle_raw_input_event(raw_key(
            KeyCode::Esc,
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ))
        .await;

        assert_eq!(app.state.effective_interaction_mode(), Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn compute_view_does_not_reconcile_an_invalid_context_menu() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.context_menu = Some(state::ContextMenuState {
            kind: state::ContextMenuKind::Workspace {
                workspace_id: app.state.workspaces[0].id.clone(),
                ws_idx: 0,
            },
            x: 2,
            y: 2,
            selected: state::ContextMenuAction::RenameWorkspace,
        });
        app.state
            .open_client_overlay(state::ClientOverlay::ContextMenu);
        app.state.workspaces.clear();
        app.state.active = None;

        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 80, 24));

        assert!(app.state.context_menu.is_some());
        assert_eq!(app.state.effective_interaction_mode(), Mode::ContextMenu);
    }

    #[test]
    fn stale_context_menu_reconciliation_restores_prefix_input_source() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.context_menu = Some(state::ContextMenuState {
            kind: state::ContextMenuKind::Workspace {
                workspace_id: app.state.workspaces[0].id.clone(),
                ws_idx: 0,
            },
            x: 2,
            y: 2,
            selected: state::ContextMenuAction::RenameWorkspace,
        });
        app.state
            .open_client_overlay(state::ClientOverlay::ContextMenu);
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);
        app.state.workspaces.clear();
        app.state.active = None;

        assert!(app.reconcile_client_interaction(true));

        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.effective_interaction_mode(), Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[tokio::test]
    async fn local_rename_overlay_takes_keys_before_the_files_dock() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(state::DockSurface::Files);
        app.state.dock_files_focused = true;
        input::open_new_tab_dialog(&mut app.state);
        app.state.name_input.clear();
        app.state.name_input_replace_on_type = false;

        app.handle_raw_input_event(raw_key(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ))
        .await;

        assert_eq!(app.state.name_input, "x");
        assert!(app.state.dock_files_filter.is_empty());
    }

    #[tokio::test]
    async fn local_rename_overlay_takes_keys_before_an_open_dock_surface_menu() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_surface_menu = Some(state::DockSurfaceMenu { selected: 0 });
        input::open_new_tab_dialog(&mut app.state);
        app.state.name_input.clear();
        app.state.name_input_replace_on_type = false;

        app.handle_raw_input_event(raw_key(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ))
        .await;

        assert_eq!(app.state.name_input, "x");
        assert_eq!(
            app.state.dock_surface_menu,
            Some(state::DockSurfaceMenu { selected: 0 })
        );
    }

    #[tokio::test]
    async fn client_local_create_overlays_preserve_another_clients_settings_mode() {
        let mut app = test_app();
        app.state.default_shell = crate::app::api::test_support::exiting_test_command().into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
        app.state.workspaces = vec![Workspace::test_new("existing")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app.state.set_server_mode(Mode::Settings);
        let mut client_a = state::SidebarPresentationState::default();
        let mut client_b = state::SidebarPresentationState::default();

        app.state.swap_sidebar_presentation(&mut client_a);
        input::open_new_workspace_dialog(&mut app.state, std::env::temp_dir());
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        app.state.swap_sidebar_presentation(&mut client_a);
        app.state.swap_sidebar_presentation(&mut client_b);
        assert_eq!(app.state.server_mode(), Mode::Settings);
        app.state.swap_sidebar_presentation(&mut client_b);

        app.state.swap_sidebar_presentation(&mut client_a);
        input::open_new_tab_dialog(&mut app.state);
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        app.state.swap_sidebar_presentation(&mut client_a);
        app.state.swap_sidebar_presentation(&mut client_b);
        assert_eq!(app.state.server_mode(), Mode::Settings);

        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn client_overlay_mouse_precedes_another_clients_server_picker() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let mut client_a = state::SidebarPresentationState::default();

        app.state.swap_sidebar_presentation(&mut client_a);
        input::open_new_tab_dialog(&mut app.state);
        app.state.name_input = "client a draft".into();
        app.state.name_input_replace_on_type = false;
        app.state.set_server_mode(Mode::WorkLinkPicker);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 30));
        let popup =
            crate::ui::centered_popup_rect(app.state.screen_rect(), 56, 7).expect("rename modal");
        let inner = ratatui::layout::Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (_, clear, _) = crate::ui::rename_button_rects(inner);

        app.handle_mouse_from_input_source(
            41,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: clear.x,
                row: clear.y,
                modifiers: KeyModifiers::empty(),
            },
        );

        assert!(app.state.name_input.is_empty());
        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        assert_eq!(app.state.effective_interaction_mode(), Mode::RenameTab);
    }

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("config.toml")
    }

    fn restore_xdg_state_home(original: Option<std::ffi::OsString>) {
        if let Some(value) = original {
            std::env::set_var("XDG_STATE_HOME", value);
        } else {
            std::env::remove_var("XDG_STATE_HOME");
        }
    }

    #[test]
    fn git_refresh_deadline_uses_hard_in_flight_deadline() {
        let mut app = test_app();
        app.state.workspaces.push(Workspace::test_new("one"));
        app.test_begin_git_refresh(1);

        assert_eq!(
            app.git_refresh_deadline(),
            app.git_refresh_in_flight.map(|refresh| refresh.deadline)
        );
    }

    #[test]
    fn unchanged_git_status_event_has_no_render_impact() {
        let mut app = test_app();
        app.test_begin_git_refresh(1);

        let changed = app.handle_internal_event_with_prefix_sync(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: Vec::new(),
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert!(!changed);
        assert!(app.git_refresh_in_flight.is_none());
    }

    #[test]
    fn tab_bar_command_events_render_only_when_visible_output_changes() {
        if !crate::platform::status_commands_supported() {
            return;
        }

        let mut app = test_app();
        app.configure_tab_bar_status(
            &[crate::config::TabBarRightEntryConfig::Command {
                command: "status".into(),
                interval_seconds: 5,
                timeout_seconds: 2,
            }],
            " ",
        );
        let generation = app.tab_bar_status_generation;
        let event = |generation, output: Option<&str>| AppEvent::TabBarCommandFinished {
            generation,
            segment_index: 0,
            result: Ok(output.map(str::to_string)),
        };

        assert!(!app.handle_internal_event_with_prefix_sync(event(generation, None)));
        assert!(app.handle_internal_event_with_prefix_sync(event(generation, Some("ready"))));
        assert!(!app.handle_internal_event_with_prefix_sync(event(generation, Some("ready"))));
        assert!(!app.handle_internal_event_with_prefix_sync(event(
            generation.wrapping_add(1),
            Some("stale"),
        )));
    }

    #[test]
    fn git_status_event_clears_in_flight_refresh() {
        let mut app = test_app();
        app.test_begin_git_refresh(1);
        let previous_refresh = Instant::now() - Duration::from_secs(10);
        app.last_git_remote_status_refresh = previous_refresh;

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: Vec::new(),
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert!(app.git_refresh_in_flight.is_none());
        assert!(app.last_git_remote_status_refresh > previous_refresh);
    }

    // ac2: an event from an invalidated generation cannot overwrite the last good branch.
    #[test]
    fn stale_git_status_event_is_dropped_by_generation() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("one");
        workspace.cached_git_branch = Some("last-good".into());
        let workspace_id = workspace.id.clone();
        let cwd = workspace.identity_cwd.clone();
        app.state.workspaces.push(workspace);
        app.git_refresh_in_flight = Some(GitRefreshInFlight {
            generation: 2,
            started_at: Instant::now(),
            deadline: Instant::now() + Duration::from_secs(2),
        });

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: vec![crate::workspace::WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd.clone(),
                status_cache_key: cwd,
                demand: crate::workspace::GitStatusRefreshDemand::ALL,
                updates_workspace_identity: true,
                auto_label: "one".into(),
                branch: Some("stale".into()),
                ahead_behind: None,
                space: None,
            }],
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert_eq!(
            app.state.workspaces[0].cached_git_branch.as_deref(),
            Some("last-good")
        );
        assert_eq!(
            app.git_refresh_in_flight.map(|refresh| refresh.generation),
            Some(2)
        );
    }

    #[test]
    fn late_git_status_event_is_dropped_at_generation_deadline() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("one");
        workspace.cached_git_branch = Some("last-good".into());
        let workspace_id = workspace.id.clone();
        let cwd = workspace.identity_cwd.clone();
        app.state.workspaces.push(workspace);
        app.test_begin_git_refresh(1);
        app.git_refresh_in_flight.as_mut().unwrap().deadline =
            Instant::now() - Duration::from_millis(1);

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: vec![crate::workspace::WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd.clone(),
                status_cache_key: cwd,
                demand: crate::workspace::GitStatusRefreshDemand::ALL,
                updates_workspace_identity: true,
                auto_label: "one".into(),
                branch: Some("late".into()),
                ahead_behind: None,
                space: None,
            }],
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert_eq!(
            app.state.workspaces[0].cached_git_branch.as_deref(),
            Some("last-good")
        );
        assert!(app.git_refresh_in_flight.is_none());
        assert!(app
            .git_refresh_deadline()
            .is_some_and(|deadline| deadline <= Instant::now()));
    }

    #[test]
    fn git_status_event_marks_render_dirty_when_status_changes() {
        let mut app = test_app();
        app.test_begin_git_refresh(1);
        app.state.workspaces.push(Workspace::test_new("one"));
        let _ = app.render_dirty.take();
        let workspace_id = app.state.workspaces[0].id.clone();
        let resolved_identity_cwd = app.state.workspaces[0].resolved_identity_cwd().unwrap();

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: vec![crate::workspace::WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: resolved_identity_cwd.clone(),
                status_cache_key: resolved_identity_cwd,
                demand: crate::workspace::GitStatusRefreshDemand::ALL,
                updates_workspace_identity: true,
                auto_label: "one".into(),
                branch: Some("render-dirty-test".into()),
                ahead_behind: Some((1, 0)),
                space: None,
            }],
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert!(app.render_dirty.is_pending());
    }

    #[test]
    fn clipboard_write_event_shows_feedback_toast() {
        let mut app = test_app();

        app.handle_internal_event(AppEvent::ClipboardWrite {
            content: b"copied".to_vec(),
        });

        assert!(app.state.toast.is_none());
        let feedback = app.state.copy_feedback.as_ref().expect("copy feedback");
        assert_eq!(feedback.message, "copied to clipboard");
        assert!(app.copy_feedback_deadline.is_some());
    }

    #[test]
    fn clipboard_feedback_can_be_disabled() {
        let mut app = test_app();
        app.state.toast_config.clipboard.enabled = false;

        app.handle_internal_event(AppEvent::ClipboardWrite {
            content: b"copied".to_vec(),
        });

        assert!(app.state.copy_feedback.is_none());
        assert!(app.copy_feedback_deadline.is_none());
    }

    #[test]
    fn clipboard_feedback_does_not_replace_notification_toast() {
        let mut app = test_app();
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".to_string(),
            context: "background · 2".to_string(),
            position: None,
            target: None,
        });
        let original_toast = app.state.toast.clone();

        app.handle_internal_event(AppEvent::ClipboardWrite {
            content: b"copied".to_vec(),
        });

        assert_eq!(app.state.toast, original_toast);
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("copied to clipboard")
        );
    }

    #[test]
    fn notification_show_api_creates_herdr_toast_with_position() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: Some("api workspace".into()),
                        position: Some(crate::config::ToastHerdrPosition::TopLeft),
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: crate::api::schema::NotificationShowReason::Shown,
            }
        );
        let toast = app.state.toast.as_ref().expect("api toast");
        assert_eq!(toast.title, "build failed");
        assert_eq!(toast.context, "api workspace");
        assert_eq!(
            toast.position,
            Some(crate::config::ToastHerdrPosition::TopLeft)
        );
        assert!(app.toast_deadline.is_some());
    }

    #[test]
    fn notification_show_api_herdr_toast_expires() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: crate::api::schema::NotificationShowReason::Shown,
            }
        );
        let deadline = app.toast_deadline.expect("api toast deadline");
        assert!(app.handle_scheduled_tasks(deadline, false));
        assert!(app.state.toast.is_none());
        assert!(app.toast_deadline.is_none());
    }

    #[test]
    fn notification_show_api_respects_off_delivery() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Off;

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: crate::api::schema::NotificationShowReason::Disabled,
            }
        );
        assert!(app.state.toast.is_none());
    }

    #[test]
    fn notification_show_api_does_not_replace_existing_toast() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".to_string(),
            context: "background · 2".to_string(),
            position: None,
            target: None,
        });

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: crate::api::schema::NotificationShowReason::Busy,
            }
        );
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("pi needs attention")
        );
    }

    #[test]
    fn notification_show_api_is_rate_limited() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.mark_api_notification_shown(Instant::now());

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: crate::api::schema::NotificationShowReason::RateLimited,
            }
        );
        assert!(app.state.toast.is_none());
    }

    #[test]
    fn unchanged_git_status_drain_has_no_render_impact() {
        let mut app = test_app();
        app.test_begin_git_refresh(1);
        app.event_tx
            .try_send(AppEvent::GitStatusRefreshed {
                generation: 1,
                results: Vec::new(),
                cache_updates: Vec::new(),
                file_fingerprints: Vec::new(),
            })
            .unwrap();

        assert!(!app.drain_internal_events());
        assert!(app.git_refresh_in_flight.is_none());
    }

    #[test]
    fn internal_event_drain_limits_work_per_tick() {
        let mut app = test_app();
        for i in 0..=APP_EVENT_DRAIN_LIMIT {
            app.event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("2.0.{i}"),
                    install_command: "herdr install".into(),
                })
                .unwrap();
        }

        assert!(app.drain_internal_events());

        let expected_version = format!("2.0.{}", APP_EVENT_DRAIN_LIMIT - 1);
        assert_eq!(
            app.state.update_available.as_deref(),
            Some(expected_version.as_str())
        );
        assert!(app.event_rx.try_recv().is_ok());
    }

    #[test]
    fn api_request_drains_all_pending_internal_events_before_reading_state() {
        let mut app = test_app();
        for i in 0..=APP_EVENT_DRAIN_LIMIT {
            app.event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("3.0.{i}"),
                    install_command: "herdr install".into(),
                })
                .unwrap();
        }

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_server_stop_after_events".into(),
            method: crate::api::schema::Method::ServerStop(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        let expected_version = format!("3.0.{APP_EVENT_DRAIN_LIMIT}");
        assert_eq!(
            app.state.update_available.as_deref(),
            Some(expected_version.as_str())
        );
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn startup_uses_configured_agent_panel_sort() {
        let mut config = Config::default();
        config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Priority);
    }

    #[test]
    fn startup_uses_configured_sidebar_state() {
        let mut config = Config::default();
        config.ui.sidebar_start_collapsed = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert!(app.state.sidebar_collapsed);
    }

    #[test]
    fn startup_uses_redraw_on_focus_gained_config() {
        let mut config = Config::default();
        config.ui.redraw_on_focus_gained = false;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert!(!app.state.redraw_on_focus_gained);
    }

    #[test]
    fn workspace_name_prompt_suppresses_default_creation_while_pending() {
        let mut app = test_app();
        app.state.prompt_new_workspace_name = true;

        app.begin_tui_workspace_create("test.workspace.create");

        assert_eq!(
            app.state.effective_interaction_mode(),
            Mode::RenameWorkspace
        );
        assert!(app.state.pending_workspace_create_cwd.is_some());
        assert!(!app.ensure_default_workspace());
        assert!(app.state.workspaces.is_empty());

        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.pending_workspace_create_cwd.is_none());
    }

    #[test]
    fn startup_uses_workspace_name_prompt_config() {
        let mut config = Config::default();
        config.ui.prompt_new_workspace_name = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert!(app.state.prompt_new_workspace_name);
    }

    #[test]
    fn theme_auto_switch_is_opt_in_and_preserves_manual_default() {
        let mut config = Config::default();
        config.theme.name = Some("tokyo-night".to_string());
        config.theme.custom = Some(crate::config::CustomThemeColors {
            light: Some(crate::config::ModeThemeColors {
                accent: Some("#010203".to_string()),
                ..Default::default()
            }),
            dark: Some(crate::config::ModeThemeColors {
                accent: Some("#040506".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert!(!app.state.theme_runtime.auto_switch);
        assert_eq!(app.state.theme_name, "tokyo-night");
        assert_eq!(app.state.palette, state::Palette::tokyo_night());
    }

    /// The regression this pins: with auto_switch on and no OSC 11 answer yet,
    /// herdr assumed a dark terminal. Guessing wrong paints one appearance's
    /// foregrounds onto the other's background — the unreadable case — and it
    /// is what pushed every host onto a hand-pinned palette. Hold the
    /// configured theme until the client actually reports.
    #[test]
    fn theme_auto_switch_holds_the_configured_theme_until_the_host_reports() {
        let mut config = Config::default();
        config.theme.name = Some("github-light-high-contrast".to_string());
        config.theme.auto_switch = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.host_terminal_appearance, None);
        assert_eq!(app.state.theme_name, "github-light-high-contrast");
        assert_eq!(
            app.state.palette,
            state::Palette::github_light_high_contrast()
        );

        assert!(app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Dark, true));
        assert_eq!(app.state.theme_name, "github-dark-high-contrast");
    }

    #[test]
    fn unknown_host_uses_effective_palette_for_pane_appearance_and_default_colors() {
        for (name, appearance, foreground, background) in [
            (
                "catppuccin",
                crate::terminal_theme::HostAppearance::Dark,
                crate::terminal_theme::RgbColor {
                    r: 205,
                    g: 214,
                    b: 244,
                },
                crate::terminal_theme::RgbColor {
                    r: 24,
                    g: 24,
                    b: 37,
                },
            ),
            (
                "catppuccin-latte",
                crate::terminal_theme::HostAppearance::Light,
                crate::terminal_theme::RgbColor {
                    r: 76,
                    g: 79,
                    b: 105,
                },
                crate::terminal_theme::RgbColor {
                    r: 239,
                    g: 241,
                    b: 245,
                },
            ),
        ] {
            let mut config = Config::default();
            config.theme.name = Some(name.to_string());
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

            assert_eq!(app.state.host_terminal_appearance, None);
            assert_eq!(app.state.pane_terminal_appearance(), appearance);
            let pane_theme = app.state.pane_terminal_theme();
            assert_eq!(pane_theme.foreground, Some(foreground));
            assert_eq!(pane_theme.background, Some(background));
        }
    }

    #[test]
    fn reported_host_defaults_take_precedence_over_palette_defaults() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let reported_foreground = crate::terminal_theme::RgbColor { r: 1, g: 2, b: 3 };
        let reported_background = crate::terminal_theme::RgbColor { r: 4, g: 5, b: 6 };
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(reported_foreground),
            background: Some(reported_background),
            ..Default::default()
        };

        let pane_theme = app.state.pane_terminal_theme();
        assert_eq!(pane_theme.foreground, Some(reported_foreground));
        assert_eq!(pane_theme.background, Some(reported_background));
    }

    fn app_with_auto_switch_theme_config() -> (crate::config::Config, App) {
        let mut config = Config::default();
        config.theme.auto_switch = true;
        config.theme.name = Some("github-light-high-contrast".to_string());
        config.theme.dark_name = Some("github-dark-high-contrast".to_string());
        config.theme.light_name = Some("github-light-high-contrast".to_string());
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        (config, app)
    }

    #[test]
    fn config_reload_keeps_an_appearance_asserted_at_runtime() {
        let (config, mut app) = app_with_auto_switch_theme_config();
        app.set_host_appearance_override(crate::config::HostAppearanceOverride::Dark);
        assert_eq!(app.state.theme_name, "github-dark-high-contrast");

        app.apply_live_config(&config, &[], &[], false);

        assert_eq!(
            app.state.theme_runtime.host_appearance,
            crate::config::HostAppearanceOverride::Dark
        );
        assert_eq!(app.state.theme_name, "github-dark-high-contrast");
    }

    #[test]
    fn config_reload_closes_a_git_menu_when_its_button_is_hidden() {
        let (mut config, mut app) = app_with_auto_switch_theme_config();
        config.ui.show_pull_button = false;
        app.state.show_pull_button = true;
        app.state.set_server_mode(Mode::GitMenu);

        app.apply_live_config(&config, &[], &[], false);

        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn config_reload_lets_a_pinned_appearance_beat_the_runtime_one() {
        let (mut config, mut app) = app_with_auto_switch_theme_config();
        app.set_host_appearance_override(crate::config::HostAppearanceOverride::Dark);
        config.theme.host_appearance = crate::config::HostAppearanceOverride::Light;

        app.apply_live_config(&config, &[], &[], false);

        assert_eq!(
            app.state.theme_runtime.host_appearance,
            crate::config::HostAppearanceOverride::Light
        );
        assert_eq!(app.state.theme_name, "github-light-high-contrast");
    }

    #[test]
    fn pane_appearance_precedence_is_override_then_host_then_palette() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        assert_eq!(
            app.state.pane_terminal_appearance(),
            crate::terminal_theme::HostAppearance::Dark
        );
        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true);
        assert_eq!(
            app.state.pane_terminal_appearance(),
            crate::terminal_theme::HostAppearance::Light
        );
        app.set_host_appearance_override(crate::config::HostAppearanceOverride::Dark);
        assert_eq!(
            app.state.pane_terminal_appearance(),
            crate::terminal_theme::HostAppearance::Dark
        );
        app.set_host_appearance_override(crate::config::HostAppearanceOverride::Auto);
        assert_eq!(
            app.state.pane_terminal_appearance(),
            crate::terminal_theme::HostAppearance::Light
        );
    }

    #[tokio::test]
    async fn appearance_override_emits_one_transition_per_change_to_each_subscribed_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        let (other_runtime, mut other_rx) = TerminalRuntime::test_with_channel(80, 24);
        for runtime in [&runtime, &other_runtime] {
            runtime
                .apply_host_terminal_appearance(Some(crate::terminal_theme::HostAppearance::Dark));
            runtime.test_process_pty_bytes(b"\x1b[?2031h");
        }
        app.terminal_runtimes
            .insert(crate::terminal::TerminalId::alloc(), runtime);
        app.terminal_runtimes
            .insert(crate::terminal::TerminalId::alloc(), other_runtime);

        assert!(app.set_host_appearance_override(crate::config::HostAppearanceOverride::Light));
        assert_eq!(
            rx.recv().await,
            Some(bytes::Bytes::from_static(b"\x1b[?997;2n"))
        );
        assert_eq!(
            other_rx.recv().await,
            Some(bytes::Bytes::from_static(b"\x1b[?997;2n"))
        );
        assert!(!app.set_host_appearance_override(crate::config::HostAppearanceOverride::Light));
        assert!(rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());

        assert!(app.set_host_appearance_override(crate::config::HostAppearanceOverride::Auto));
        assert_eq!(
            rx.recv().await,
            Some(bytes::Bytes::from_static(b"\x1b[?997;1n"))
        );
        assert_eq!(
            other_rx.recv().await,
            Some(bytes::Bytes::from_static(b"\x1b[?997;1n"))
        );
        assert!(rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn reload_config_applies_host_appearance_override_to_ui_and_subscribed_panes() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-host-appearance");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[theme]\nname = \"catppuccin\"\nauto_switch = true\nhost_appearance = \"light\"\n",
        )
        .unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = test_app();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        runtime.apply_host_terminal_appearance(Some(crate::terminal_theme::HostAppearance::Dark));
        runtime.test_process_pty_bytes(b"\x1b[?2031h");
        app.terminal_runtimes
            .insert(crate::terminal::TerminalId::alloc(), runtime);

        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.theme_name, "catppuccin-latte");
        assert_eq!(
            app.state.palette.appearance(),
            Some(crate::terminal_theme::HostAppearance::Light)
        );
        assert_eq!(
            rx.recv().await,
            Some(bytes::Bytes::from_static(b"\x1b[?997;2n"))
        );
        assert!(rx.try_recv().is_err());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A pinned palette that contradicts the terminal is legal but nearly
    /// always a mistake, and it is invisible precisely because it hides the UI.
    /// Name it where the operator already looks: the config-reload report.
    #[test]
    fn a_palette_that_contradicts_the_terminal_is_reported() {
        let mut config = Config::default();
        config.theme.name = Some("github-light-high-contrast".to_string());
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.theme_appearance_mismatch, None);

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Dark, true);
        let mismatch = app
            .state
            .theme_appearance_mismatch
            .clone()
            .expect("light palette in front of a dark terminal is a mismatch");
        assert!(
            mismatch.contains("github-light-high-contrast"),
            "{mismatch}"
        );
        assert!(mismatch.contains("dark background"), "{mismatch}");

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true);
        assert_eq!(app.state.theme_appearance_mismatch, None);
    }

    /// The mismatch describes the terminal in front of the config, not a fault
    /// in the config: it must reach the reload report without demoting a valid
    /// reload to `Partial` or taking over the config diagnostic banner.
    #[test]
    fn a_reported_mismatch_does_not_demote_a_valid_config_reload() {
        let mut config = Config::default();
        config.theme.name = Some("github-light-high-contrast".to_string());
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Dark, true);

        let report = app.apply_live_config(&config, &[], &[], false);

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(report.diagnostics.len(), 1);
        assert!(
            report.diagnostics[0].contains("github-light-high-contrast"),
            "{:?}",
            report.diagnostics
        );
        assert_eq!(app.state.config_diagnostic, None);
    }

    #[test]
    fn theme_auto_switch_uses_sibling_map_and_explicit_appearance() {
        let mut config = Config::default();
        config.theme.name = Some("tokyo-night".to_string());
        config.theme.auto_switch = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.theme_name, "tokyo-night");
        assert!(
            app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true)
        );

        assert_eq!(app.state.theme_name, "tokyo-night-day");
        assert_eq!(app.state.palette, state::Palette::tokyo_night_day());
    }

    #[test]
    fn theme_auto_switch_applies_custom_overrides_after_active_base() {
        let mut config = Config::default();
        config.theme.name = Some("gruvbox".to_string());
        config.theme.auto_switch = true;
        config.theme.custom = Some(crate::config::CustomThemeColors {
            accent: Some("#010203".to_string()),
            ..Default::default()
        });
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true);

        assert_eq!(app.state.theme_name, "gruvbox-light");
        assert_eq!(
            app.state.palette.accent,
            ratatui::style::Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn theme_auto_switch_layers_active_mode_overrides_last() {
        let mut config = Config::default();
        config.theme.name = Some("gruvbox".to_string());
        config.theme.auto_switch = true;
        config.theme.custom = Some(crate::config::CustomThemeColors {
            accent: Some("#010203".to_string()),
            text: Some("#040506".to_string()),
            light: Some(crate::config::ModeThemeColors {
                accent: Some("#070809".to_string()),
                ..Default::default()
            }),
            dark: Some(crate::config::ModeThemeColors {
                text: Some("#0a0b0c".to_string()),
                sidebar_bg: Some("#0d0e0f".to_string()),
                active_row_bg: Some("#101112".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        // No OSC 11 answer has arrived yet, so the base custom overrides apply
        // and neither mode block is layered on. This fork does not guess an
        // appearance; see `resolve_effective_theme`.
        assert_eq!(
            app.state.palette.accent,
            ratatui::style::Color::Rgb(1, 2, 3)
        );
        assert_eq!(app.state.palette.text, ratatui::style::Color::Rgb(4, 5, 6));
        assert_eq!(app.state.palette.sidebar_bg, None);

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Dark, true);

        assert_eq!(
            app.state.palette.accent,
            ratatui::style::Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            app.state.palette.text,
            ratatui::style::Color::Rgb(10, 11, 12)
        );
        assert_eq!(
            app.state.palette.sidebar_bg,
            Some(ratatui::style::Color::Rgb(13, 14, 15))
        );
        assert_eq!(
            app.state.palette.active_row_bg,
            ratatui::style::Color::Rgb(16, 17, 18)
        );

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true);

        assert_eq!(
            app.state.palette.accent,
            ratatui::style::Color::Rgb(7, 8, 9)
        );
        assert_eq!(app.state.palette.text, ratatui::style::Color::Rgb(4, 5, 6));
    }

    #[test]
    fn inferred_background_appearance_does_not_override_explicit_report() {
        let mut config = Config::default();
        config.theme.name = Some("catppuccin".to_string());
        config.theme.auto_switch = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Dark, true);
        app.update_host_terminal_theme(
            crate::terminal_theme::DefaultColorKind::Background,
            crate::terminal_theme::RgbColor {
                r: 0xff,
                g: 0xff,
                b: 0xff,
            },
        );

        assert_eq!(
            app.state.host_terminal_appearance,
            Some(crate::terminal_theme::HostAppearance::Dark)
        );
        assert_eq!(app.state.theme_name, "catppuccin");
    }

    #[test]
    fn startup_restores_preview_update_available_from_saved_notes() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("startup-preview-update-available");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        // Use a bogus far-future version so preview=true regardless of current binary version.
        crate::release_notes::save_pending("99.99.99", "### Changed\n- One").unwrap();

        let app = test_app();

        assert_eq!(app.state.update_available.as_deref(), Some("99.99.99"));
        assert!(app.state.latest_release_notes_available);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn startup_does_not_restore_update_available_from_older_saved_notes() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("startup-stale-update-notes");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        crate::release_notes::save_pending("0.4.9", "### Changed\n- One").unwrap();

        let app = test_app();

        assert_eq!(app.state.update_available, None);
        assert!(app.state.latest_release_notes_available);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn startup_keeps_pending_release_notes_available_without_auto_opening() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("startup-pending-release-notes-no-auto-open");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        crate::release_notes::save_pending(env!("CARGO_PKG_VERSION"), "### Changed\n- One")
            .unwrap();
        let config = Config {
            onboarding: Some(false),
            ..Default::default()
        };
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.server_mode(), Mode::Navigate);
        assert!(app.state.release_notes.is_none());
        assert!(app.state.latest_release_notes_available);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn startup_still_auto_opens_unseen_product_announcement() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("startup-product-announcement-auto-open");
        let state_home = path.parent().unwrap().join("state");
        let original_xdg_state_home = std::env::var_os("XDG_STATE_HOME");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        env.set("XDG_STATE_HOME", &state_home);

        crate::release_notes::save_pending(env!("CARGO_PKG_VERSION"), "### Changed\n- One")
            .unwrap();
        crate::product_announcements::save_manifest_announcement(
            env!("CARGO_PKG_VERSION"),
            Some(&crate::product_announcements::ManifestAnnouncement {
                id: "startup-announcement".into(),
                title: Some("Startup announcement".into()),
                body: "### Announcement\n- One".into(),
            }),
        )
        .unwrap();

        let config = Config {
            onboarding: Some(false),
            ..Default::default()
        };
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.server_mode(), Mode::ProductAnnouncement);
        assert_eq!(
            app.state
                .product_announcement
                .as_ref()
                .map(|announcement| announcement.id.as_str()),
            Some("startup-announcement")
        );
        assert!(app.state.release_notes.is_none());

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        restore_xdg_state_home(original_xdg_state_home);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_live_state() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-success");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[terminal]\ndefault_shell = \"nu\"\nshell_mode = \"non_login\"\nnew_cwd = \"home\"\n[keys]\nnew_workspace = \"prefix+m\"\nprefix = \"ctrl+a\"\n[update]\nversion_check = false\nmanifest_check = false\n[server]\nheadless_cols = 160\nheadless_rows = 50\n[remote.fleet]\nself_name = \"laptop\"\n[ui]\nagent_panel_sort = \"priority\"\nredraw_on_focus_gained = false\ncopy_on_select = false\nright_click_passthrough_modifier = \"ctrl\"\nprompt_new_workspace_name = true\n[ui.toast]\ndelivery = \"herdr\"\n[experimental]\nswitch_ascii_input_source_in_prefix = true\n",
        )
        .unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let selection_pane = crate::layout::PaneId::alloc();
        app.state.selection = Some(crate::selection::Selection::range(
            selection_pane,
            0,
            0,
            1,
            None,
        ));
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 1,
            last_mouse_screen_row: 1,
            inner_rect: ratatui::layout::Rect::new(0, 0, 2, 2),
        });
        let selection_deadline = Instant::now();
        app.selection_autoscroll_deadline = Some(selection_deadline);
        app.selection_highlight_clear_deadline = Some(selection_deadline);
        app.last_pane_click = Some(PaneClickState {
            pane_id: selection_pane,
            viewport_row: 0,
            col: 0,
            at: selection_deadline,
        });
        app.next_auto_update_check = Some(Instant::now());
        app.next_agent_manifest_update_check = Some(Instant::now());
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.headless_size, (160, 50));
        assert_eq!(app.state.agent_host_name, "laptop");
        assert_eq!(app.state.prefix_code, KeyCode::Char('a'));
        assert_eq!(app.state.prefix_mods, KeyModifiers::CONTROL);
        assert!(app
            .state
            .keybinds
            .new_workspace
            .matches_prefix(&KeyEvent::new(KeyCode::Char('m'), KeyModifiers::empty())));
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        );
        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Priority);
        assert!(!app.state.redraw_on_focus_gained);
        assert!(!app.state.copy_on_select);
        assert!(app.state.prompt_new_workspace_name);
        assert!(app.state.selection.is_some());
        assert!(app.state.selection_autoscroll.is_some());
        assert_eq!(app.selection_autoscroll_deadline, Some(selection_deadline));
        assert_eq!(
            app.selection_highlight_clear_deadline,
            Some(selection_deadline)
        );
        assert!(app.last_pane_click.is_some());

        app.state.set_server_mode(Mode::Copy);
        app.state.selection = Some(crate::selection::Selection::range(
            selection_pane,
            0,
            0,
            1,
            None,
        ));
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert!(app.state.selection.is_some());
        assert_eq!(
            app.state.right_click_passthrough_modifiers,
            Some(KeyModifiers::CONTROL)
        );
        assert!(app.state.request_client_config_reload);
        assert_eq!(app.state.default_shell, "nu");
        assert_eq!(
            app.state.shell_mode,
            crate::config::ShellModeConfig::NonLogin
        );
        assert_eq!(
            app.state.new_terminal_cwd,
            crate::config::NewTerminalCwdConfig::Home
        );
        assert!(!app.update_version_check_enabled);
        assert!(!app.update_manifest_check_enabled);
        assert!(app.next_auto_update_check.is_none());
        assert!(app.next_agent_manifest_update_check.is_none());
        assert!(app.state.switch_ascii_input_source_in_prefix);
        assert!(app.state.config_diagnostic.is_none());
        let toast = app.state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, crate::app::state::ToastKind::UpdateInstalled);
        assert_eq!(toast.title, "reloaded config");
        assert_eq!(toast.context, "using config.toml");

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn status_context_reenable_projects_mutated_cached_focus_without_io() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-enable-status-focus");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[ui.status_bar]\nenabled = true\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut startup_config = Config::default();
        startup_config.ui.status_bar.enabled = false;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &startup_config,
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("reload-status-focus");
        let nested = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(nested);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let nested_terminal = app.state.workspaces[0]
            .terminal_id(nested)
            .expect("nested terminal")
            .clone();
        app.state.terminals.get_mut(&nested_terminal).unwrap().cwd = "/repo/old".into();
        let (runtime, _input_rx) = TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(nested_terminal.clone(), runtime);
        app.last_focus = Some((0, nested));
        assert!(app.sync_status_context_before_render());
        app.state.status_git_cwd = Some("/repo/old".into());
        app.state.status_git_branch = Some("old-branch".into());
        app.state.terminals.get_mut(&nested_terminal).unwrap().cwd = "/repo/nested".into();
        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();

        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert!(app.state.status_bar_enabled);
        assert!(app.state.status_focus_projection_initialized);
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/repo/nested")));
        assert_eq!(branch, None);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
        assert!(app.git_identity_refresh_requested);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn status_context_reenable_invalidates_same_cwd_stale_branch_without_io() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-enable-status-same-cwd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[ui.status_bar]\nenabled = true\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut startup_config = Config::default();
        startup_config.ui.status_bar.enabled = false;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &startup_config,
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("reload-status-same-cwd")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.workspaces[0]
            .terminal_id(pane)
            .expect("focused terminal")
            .clone();
        app.state.terminals.get_mut(&terminal).unwrap().cwd = "/repo".into();
        let (runtime, _input_rx) = TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal, runtime);
        app.last_focus = Some((0, pane));
        assert!(app.sync_status_context_before_render());
        app.state.status_git_cwd = Some("/repo".into());
        app.state.status_git_branch = Some("old-branch".into());
        app.state.workspaces[0].cached_git_branch = Some("new-branch".into());
        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();

        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/repo")));
        assert_eq!(branch, None);
        assert_eq!(app.state.status_git_cwd, None);
        assert_eq!(app.state.status_git_branch, None);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
        assert!(app.git_identity_refresh_requested);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_requests_client_reload_for_host_cursor_only_change() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-host-cursor");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[ui]\nhost_cursor = \"native\"\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.state.request_client_config_reload = false;

        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(
            app.loaded_host_cursor,
            crate::config::HostCursorModeConfig::Native
        );
        assert!(app.state.request_client_config_reload);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_sidebar_width_only_when_config_owned() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-sidebar-width");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.sidebar_width_source,
            state::SidebarWidthSource::ConfigDefault
        );

        std::fs::write(&path, "[ui]\nsidebar_width = 34\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.default_sidebar_width, 34);
        assert_eq!(app.state.sidebar_width, 34);

        app.state.sidebar_width = 31;
        app.state.sidebar_width_source = state::SidebarWidthSource::Manual;
        std::fs::write(&path, "[ui]\nsidebar_width = 35\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.default_sidebar_width, 35);
        assert_eq!(app.state.sidebar_width, 31);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_sidebar_token_rows() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-sidebar-tokens");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = test_app();
        let previous_revision = app.state.sidebar_projection_revision;

        std::fs::write(
            &path,
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"$summary\"]]\nrow_gap = 1\n\n[ui.sidebar.agents.rows_by_agent]\nclaude = [[\"terminal_title_stripped\"]]\n\n[ui.sidebar.spaces]\nrows = [[\"workspace\", \"$jj_status\"]]\nrow_gap = 3\n",
        )
        .unwrap();
        app.state.workspace_scroll = 5;
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.workspace_scroll, 0);
        assert_eq!(app.state.sidebar_projection_revision, previous_revision + 1);
        assert_eq!(
            app.state.sidebar_agents.rows,
            vec![vec![
                crate::config::AgentSidebarToken::StateIcon,
                crate::config::AgentSidebarToken::Custom("summary".into()),
            ]]
        );
        assert_eq!(
            app.state.sidebar_agents.rows_by_agent["claude"],
            vec![vec![
                crate::config::AgentSidebarToken::TerminalTitleStripped,
            ]]
        );
        assert_eq!(app.state.sidebar_agents.row_gap, 1);
        assert_eq!(
            app.state.sidebar_spaces.rows,
            vec![vec![
                crate::config::SpaceSidebarToken::Workspace,
                crate::config::SpaceSidebarToken::Custom("jj_status".into()),
            ]]
        );
        assert_eq!(app.state.sidebar_spaces.row_gap, 3);

        let previous_agents = app.state.sidebar_agents.clone();
        let previous_revision = app.state.sidebar_projection_revision;
        std::fs::write(
            &path,
            "[ui.sidebar.agents]\nrows = [[\"agent\"]]\n\n[ui.sidebar.agents.rows_by_agent]\nclaude-code = [[\"terminal_title\"]]\n",
        )
        .unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert_eq!(app.state.sidebar_agents, previous_agents);
        assert_eq!(app.state.sidebar_projection_revision, previous_revision);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_does_not_reset_sidebar_to_startup_state() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-sidebar-start-collapsed");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert!(!app.state.sidebar_collapsed);

        std::fs::write(&path, "[ui]\nsidebar_start_collapsed = true\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert!(!app.state.sidebar_collapsed);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_sidebar_collapsed_mode() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-sidebar-collapsed-mode");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.sidebar_collapsed_mode,
            crate::config::SidebarCollapsedModeConfig::Compact
        );

        std::fs::write(&path, "[ui]\nsidebar_collapsed_mode = \"hidden\"\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(
            app.state.sidebar_collapsed_mode,
            crate::config::SidebarCollapsedModeConfig::Hidden
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_settled_read_only_grace() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-settled-read-only-grace");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let mut app = test_app();
        assert_eq!(
            app.state.settled_read_only_after,
            std::time::Duration::from_secs(900)
        );

        std::fs::write(&path, "[session]\nsettled_read_only_after_minutes = 0\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.settled_read_only_after, std::time::Duration::ZERO);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_sidebar_bounds_and_reclamps() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-sidebar-bounds");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        // Default bounds.
        assert_eq!(app.state.sidebar_min_width, 18);
        assert_eq!(app.state.sidebar_max_width, 36);
        assert_eq!(
            app.state.mobile_width_threshold,
            crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD
        );

        // Manually set a width and flip the source so the existing
        // sidebar_width-only-when-config-owned guard does NOT update it.
        app.state.sidebar_width = 30;
        app.state.sidebar_width_source = state::SidebarWidthSource::Manual;

        // Tightening max below the current width must re-clamp the live width
        // even when source is Manual — bounds always apply.
        std::fs::write(&path, "[ui]\nsidebar_max_width = 24\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.sidebar_max_width, 24);
        assert_eq!(
            app.state.sidebar_width, 24,
            "manual width must re-clamp to new max"
        );

        // Loosening max leaves the live width alone (it's already within bounds).
        app.state.sidebar_width = 24;
        std::fs::write(&path, "[ui]\nsidebar_max_width = 60\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.sidebar_max_width, 60);
        assert_eq!(app.state.sidebar_width, 24);

        // Raising min above the current width re-clamps upward.
        std::fs::write(&path, "[ui]\nsidebar_min_width = 30\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.sidebar_min_width, 30);
        assert_eq!(
            app.state.sidebar_width, 30,
            "manual width must re-clamp up to new min"
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_mobile_width_threshold() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-mobile-width-threshold");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.mobile_width_threshold,
            crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD
        );

        std::fs::write(&path, "[ui]\nmobile_width_threshold = 96\n").unwrap();
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.mobile_width_threshold, 96);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn app_new_falls_back_to_default_bounds_on_inverted_config() {
        let mut config = Config::default();
        config.ui.sidebar_min_width = 50;
        config.ui.sidebar_max_width = 30;

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(
            app.state.sidebar_min_width, 18,
            "App::new must fall back to default min when bounds are inverted"
        );
        assert_eq!(
            app.state.sidebar_max_width, 36,
            "App::new must fall back to default max when bounds are inverted"
        );
    }

    #[test]
    fn reload_config_invalid_sidebar_bounds_keeps_previous_ui_and_returns_partial() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-invalid-sidebar-bounds");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_min = app.state.sidebar_min_width;
        let original_max = app.state.sidebar_max_width;
        let original_mouse_capture = app.state.mouse_capture;
        // Pair the bad bounds with another `[ui]` field change to confirm the
        // entire section is treated as invalid (not just the bounds).
        let target_mouse_capture = !original_mouse_capture;
        std::fs::write(
            &path,
            format!(
                "[ui]\nsidebar_min_width = 50\nsidebar_max_width = 30\nmouse_capture = {}\n",
                target_mouse_capture
            ),
        )
        .unwrap();

        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("sidebar_min_width")
                && diagnostic.contains("sidebar_max_width")
                && diagnostic.contains("greater")
        }));
        assert_eq!(app.state.sidebar_min_width, original_min);
        assert_eq!(app.state.sidebar_max_width, original_max);
        assert_eq!(
            app.state.mouse_capture, original_mouse_capture,
            "[ui] is treated as invalid on bad bounds; mouse_capture must not apply"
        );
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("config.toml; herdr config check")
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_disables_invalid_binding_but_applies_valid_keymap_and_other_sections() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-invalid-keybind");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[keys]\nnew_workspace = \"wat\"\n[ui.toast]\ndelivery = \"terminal\"\n",
        )
        .unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_prefix = (app.state.prefix_code, app.state.prefix_mods);
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("keys.new_workspace") && diagnostic.contains("disabling binding")
        }));
        assert_eq!(
            (app.state.prefix_code, app.state.prefix_mods),
            original_prefix
        );
        assert!(app.state.keybinds.new_workspace.bindings.is_empty());
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Terminal
        );
        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_applies_known_sibling_and_summarizes_unknown_key() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-unknown-key");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let target_mouse_capture = !app.state.mouse_capture;
        std::fs::write(
            &path,
            format!("[ui]\nmouse_capture = {target_mouse_capture}\nmouse_captur = false\n"),
        )
        .unwrap();

        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert_eq!(
            report.diagnostics,
            vec!["unknown config key ui.mouse_captur; ignoring key"]
        );
        assert_eq!(app.state.mouse_capture, target_mouse_capture);
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("config.toml has unknown keys; herdr config check")
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_user_binding_displaces_default_without_rejecting_prefix() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-user-binding-displaces-default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[keys]\nprefix = \"ctrl+space\"\nprevious_workspace = \"prefix+shift+l\"\n",
        )
        .unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.prefix_code, KeyCode::Char(' '));
        assert_eq!(app.state.prefix_mods, KeyModifiers::CONTROL);
        assert!(app
            .state
            .keybinds
            .previous_workspace
            .matches_prefix(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::SHIFT)));
        assert!(app.state.keybinds.swap_pane_right.bindings.is_empty());
        assert!(app.state.config_diagnostic.is_none());

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_preserves_invalid_ui_section_but_applies_valid_keys() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-invalid-ui-section");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[keys]\nnew_workspace = \"prefix+m\"\n[ui.toast]\ndelivery = \"desktop\"\n",
        )
        .unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("invalid ui config")));
        assert!(app
            .state
            .keybinds
            .new_workspace
            .matches_prefix(&KeyEvent::new(KeyCode::Char('m'), KeyModifiers::empty())));
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        );
        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_preserves_invalid_terminal_section_but_applies_valid_ui() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-invalid-terminal-section");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[terminal]\ndefault_shell = \"nu\"\nshell_mode = \"sideways\"\nnew_cwd = \"home\"\n[ui.toast]\ndelivery = \"terminal\"\n",
        )
        .unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_default_shell = app.state.default_shell.clone();
        let original_shell_mode = app.state.shell_mode;
        let original_new_cwd = app.state.new_terminal_cwd.clone();
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("invalid terminal config")));
        assert_eq!(app.state.default_shell, original_default_shell);
        assert_eq!(app.state.shell_mode, original_shell_mode);
        assert_eq!(app.state.new_terminal_cwd, original_new_cwd);
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Terminal
        );
        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn settings_save_toast_delivery_persists_then_applies_live_config() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("settings-save-toast-delivery");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "onboarding = false\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Off
        );

        app.save_toast_delivery(crate::config::ToastDelivery::Terminal);

        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Terminal
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("delivery = \"terminal\""));
        assert!(app.state.config_diagnostic.is_none());

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn settings_source_control_edits_reach_live_consumers_without_restart() {
        use crate::app::settings_general::ConfigEdit;

        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("settings-source-control-live-reload");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[source_control]\nmerge_method = \"merge\"\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        for (section, key, value) in [
            ("source_control", "commit_message_model", "claude-opus-5"),
            ("source_control", "branch_prefix", "live/"),
            ("worktrees", "directory", "/tmp/herdr-live-worktrees"),
        ] {
            app.save_config_edit(ConfigEdit::Text {
                section,
                key,
                value: value.into(),
            });
        }
        app.save_config_edit(ConfigEdit::Bool {
            section: "source_control",
            key: "commit_stage_all",
            value: true,
        });
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(
            app.state.pr_merge_method,
            crate::config::MergeMethodConfig::Merge
        );
        assert!(crate::app::git_actions::wrapped_command(
            state::GitAction::Commit,
            &app.state.commit_message_model,
            app.state.commit_stage_all,
        )
        .contains("claude-opus-5"));
        assert!(app.state.commit_stage_all);
        assert_eq!(
            crate::ui::work_list_detail::ticket_worktree_branch(
                &app.state.branch_prefix,
                "T3-9F",
                "Live reload",
            ),
            "live/t3-9f-live-reload"
        );
        assert_eq!(
            crate::worktree::default_checkout_path(
                &app.state.worktree_directory,
                "herdr",
                "live/t3-9f-live-reload",
            ),
            std::path::PathBuf::from("/tmp/herdr-live-worktrees/herdr/live-t3-9f-live-reload")
        );

        std::fs::write(
            &path,
            "[source_control]\nmerge_method = \"squash\"\ncommit_message_model = 17\nbranch_prefix = \"ignored/\"\n[worktrees]\ndirectory = \"/tmp/herdr-live-worktrees\"\n",
        )
        .unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("invalid source control config")));
        assert_eq!(
            app.state.pr_merge_method,
            crate::config::MergeMethodConfig::Merge
        );
        assert_eq!(app.state.commit_message_model, "claude-opus-5");
        assert_eq!(app.state.branch_prefix, "live/");

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn working_row_opacity_general_edit_persists_and_reloads_live() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("working-row-opacity");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "onboarding = false\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(app.state.working_row_opacity_percent, 100);
        app.save_config_edit(crate::app::settings_general::ConfigEdit::Integer {
            section: "ui",
            key: "working_row_opacity_percent",
            value: 75,
        });
        assert_eq!(app.state.working_row_opacity_percent, 75);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("working_row_opacity_percent = 75"));

        std::fs::write(&path, "[ui]\nworking_row_opacity_percent = 50\n").unwrap();
        app.apply_config_from_disk(false);
        assert_eq!(app.state.working_row_opacity_percent, 50);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_status_indicators_persists_then_applies_live_config() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("save-status-indicators");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "onboarding = false\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.status_indicators,
            crate::config::StatusIndicatorStyle::Dots
        );

        app.save_status_indicators(crate::config::StatusIndicatorStyle::Symbols);

        assert_eq!(
            app.state.status_indicators,
            crate::config::StatusIndicatorStyle::Symbols
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("status_indicators = \"symbols\""));
        assert!(app.state.config_diagnostic.is_none());

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_agent_panel_sort_persists_then_applies_live_config() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("save-agent-panel-sort");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "onboarding = false\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Spaces);

        app.save_agent_panel_sort(state::AgentPanelSort::Priority);

        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Priority);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("agent_panel_sort = \"priority\""));
        assert!(app.state.config_diagnostic.is_none());

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_keeps_current_state_on_invalid_toml() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("reload-config-invalid-toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[keys\nnew_workspace = \"g\"\n").unwrap();
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_prefix = (app.state.prefix_code, app.state.prefix_mods);
        let original_keybinds = app.state.keybinds.new_workspace.clone();
        let original_toast_delivery = app.state.toast_config.delivery;
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Failed);
        assert_eq!(
            (app.state.prefix_code, app.state.prefix_mods),
            original_prefix
        );
        assert_eq!(app.state.keybinds.new_workspace, original_keybinds);
        assert_eq!(app.state.toast_config.delivery, original_toast_delivery);
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| {
                message == "config.toml invalid; keeping current config; herdr config check"
            }));
        assert!(app.state.toast.is_none());

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn raw_input_waits_when_reader_is_gone() {
        let result =
            tokio::time::timeout(Duration::from_millis(20), recv_raw_input_or_pending(None)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn terminal_mode_handles_repeat_key_events() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Backspace,
                KeyModifiers::empty(),
                KeyEventKind::Repeat,
            ))
            .await;

        assert!(handled);
    }

    #[tokio::test]
    async fn monolithic_input_forwards_report_all_printable_event_kinds() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 4);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        for kind in [
            KeyEventKind::Press,
            KeyEventKind::Repeat,
            KeyEventKind::Release,
        ] {
            app.handle_raw_input_event(raw_key(KeyCode::Char('j'), KeyModifiers::empty(), kind))
                .await;
        }

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:2u")
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn outer_focus_gained_marks_only_focused_done_pane_seen() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        let split_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let background_tab = workspace.test_add_tab(Some("background"));
        let background_pane = workspace.tabs[background_tab].root_pane;

        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let root_terminal_id = app.state.workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&root_terminal_id)
            .unwrap()
            .set_raw_agent_state_for_test(AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&root_pane)
            .unwrap()
            .seen = false;
        let split_terminal_id = app.state.workspaces[0].tabs[0].panes[&split_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&split_terminal_id)
            .unwrap()
            .set_raw_agent_state_for_test(AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&split_pane)
            .unwrap()
            .seen = false;
        let bg_terminal_id = app.state.workspaces[0].tabs[background_tab].panes[&background_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&bg_terminal_id)
            .unwrap()
            .set_raw_agent_state_for_test(AgentState::Idle);
        app.state.workspaces[0].tabs[background_tab]
            .panes
            .get_mut(&background_pane)
            .unwrap()
            .seen = false;

        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.outer_terminal_focus = Some(false);
        let focused_pane = app.state.workspaces[0].focused_pane_id().unwrap();

        let handled = app
            .handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
            .await;

        assert!(handled);
        assert_eq!(app.state.outer_terminal_focus, Some(true));
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&root_pane].seen,
            root_pane == focused_pane
        );
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&split_pane].seen,
            split_pane == focused_pane
        );
        assert!(!app.state.workspaces[0].tabs[background_tab].panes[&background_pane].seen);
    }

    #[tokio::test]
    async fn outer_focus_gained_does_not_require_full_redraw_when_disabled() {
        let mut app = test_app();
        app.state.redraw_on_focus_gained = false;

        let handled = app
            .handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
            .await;

        assert!(handled);
        assert_eq!(app.state.outer_terminal_focus, Some(true));
        assert!(!app.full_redraw_pending);
    }

    #[tokio::test]
    async fn monolithic_outer_focus_events_reach_reporting_pane() {
        let (mut app, mut input_rx) = focus_reporting_app();

        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
                .await
        );
        assert_eq!(
            input_rx
                .recv()
                .await
                .expect("forwarded focus gained report"),
            bytes::Bytes::from_static(b"\x1b[I")
        );

        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusLost)
                .await
        );
        assert_eq!(
            input_rx.recv().await.expect("forwarded focus lost report"),
            bytes::Bytes::from_static(b"\x1b[O")
        );
    }

    #[tokio::test]
    async fn symphony_overlay_blocks_monolithic_outer_focus_events() {
        let (mut app, mut input_rx) = focus_reporting_app();
        app.state.toggle_symphony();

        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
                .await
        );
        assert_eq!(app.state.outer_terminal_focus, Some(true));
        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusLost)
                .await
        );
        assert_eq!(app.state.outer_terminal_focus, Some(false));
        assert!(input_rx.try_recv().is_err());

        app.state.toggle_symphony();
        app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
            .await;
        app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusLost)
            .await;

        assert_eq!(
            input_rx
                .try_recv()
                .expect("focus gained after overlay closes"),
            bytes::Bytes::from_static(b"\x1b[I")
        );
        assert_eq!(
            input_rx
                .try_recv()
                .expect("focus lost after overlay closes"),
            bytes::Bytes::from_static(b"\x1b[O")
        );
    }

    #[tokio::test]
    async fn symphony_overlay_blocks_headless_outer_focus_events() {
        let (mut app, mut input_rx) = focus_reporting_app();
        app.state.toggle_symphony();

        app.route_client_events(
            vec![
                crate::raw_input::RawInputEvent::OuterFocusGained,
                crate::raw_input::RawInputEvent::OuterFocusLost,
            ],
            false,
        );
        assert!(input_rx.try_recv().is_err());

        app.state.toggle_symphony();
        app.route_client_events(
            vec![
                crate::raw_input::RawInputEvent::OuterFocusGained,
                crate::raw_input::RawInputEvent::OuterFocusLost,
            ],
            false,
        );

        assert_eq!(
            input_rx
                .try_recv()
                .expect("focus gained after overlay closes"),
            bytes::Bytes::from_static(b"\x1b[I")
        );
        assert_eq!(
            input_rx
                .try_recv()
                .expect("focus lost after overlay closes"),
            bytes::Bytes::from_static(b"\x1b[O")
        );
    }

    #[tokio::test]
    async fn outer_focus_events_reconcile_pending_pane_focus() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("focus-transition");
        let previous_pane = workspace.tabs[0].root_pane;
        let next_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(previous_pane);
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[?1004h",
                4,
            );
        workspace.insert_test_runtime(next_pane, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.last_focus = Some((0, previous_pane));

        assert!(app.state.focus_pane_in_workspace(0, next_pane));
        assert!(
            !app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusLost)
                .await
        );
        app.sync_focus_events();
        assert_eq!(
            input_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[O")
        );
        assert!(input_rx.try_recv().is_err());

        assert!(app.state.focus_pane_in_workspace(0, previous_pane));
        app.sync_focus_events();
        assert_eq!(
            input_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[O")
        );

        assert!(app.state.focus_pane_in_workspace(0, next_pane));
        app.sync_focus_events();
        assert_eq!(
            input_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[O")
        );

        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
                .await
        );
        assert_eq!(
            input_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[I")
        );

        assert!(app.state.focus_pane_in_workspace(0, previous_pane));
        app.sync_focus_events();
        assert_eq!(
            input_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[O")
        );
        assert!(app.state.focus_pane_in_workspace(0, next_pane));
        app.sync_focus_events();
        assert_eq!(
            input_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[I")
        );
    }

    #[tokio::test]
    async fn repeat_key_events_are_ignored_outside_terminal_mode() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::ReleaseNotes);
        app.state.release_notes = Some(release_notes_state());

        let handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Repeat,
            ))
            .await;

        assert!(!handled);
        assert_eq!(app.state.server_mode(), Mode::ReleaseNotes);
        assert!(app.state.release_notes.is_some());
    }

    #[tokio::test]
    async fn modal_press_does_not_leak_repeat_into_terminal_mode() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::ReleaseNotes);
        app.state.release_notes = Some(release_notes_state());

        let press_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Press,
            ))
            .await;
        let repeat_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Repeat,
            ))
            .await;
        let release_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Release,
            ))
            .await;
        let next_press_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Press,
            ))
            .await;

        assert!(press_handled);
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert!(!repeat_handled);
        assert!(!release_handled);
        assert!(next_press_handled);
    }

    #[test]
    fn read_only_api_requests_do_not_force_rerender() {
        let read_only = crate::api::schema::Request {
            id: "req_1".into(),
            method: crate::api::schema::Method::WorkspaceList(
                crate::api::schema::EmptyParams::default(),
            ),
        };
        let mutating = crate::api::schema::Request {
            id: "req_2".into(),
            method: crate::api::schema::Method::WorkspaceFocus(
                crate::api::schema::WorkspaceTarget {
                    workspace_id: "w1".into(),
                },
            ),
        };
        let pane_rename = crate::api::schema::Request {
            id: "req_3".into(),
            method: crate::api::schema::Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id: "w1:p1".into(),
                label: Some("logs".into()),
            }),
        };
        let worktree_list = crate::api::schema::Request {
            id: "req_4".into(),
            method: crate::api::schema::Method::WorktreeList(
                crate::api::schema::WorktreeListParams::default(),
            ),
        };
        let worktree_create = crate::api::schema::Request {
            id: "req_5".into(),
            method: crate::api::schema::Method::WorktreeCreate(
                crate::api::schema::WorktreeCreateParams::default(),
            ),
        };
        let pane_swap = crate::api::schema::Request {
            id: "req_6".into(),
            method: crate::api::schema::Method::PaneSwap(crate::api::schema::PaneSwapParams {
                pane_id: Some("w1:p1".into()),
                direction: Some(crate::api::schema::PaneDirection::Right),
                ..crate::api::schema::PaneSwapParams::default()
            }),
        };
        let pane_focus_direction = crate::api::schema::Request {
            id: "req_7".into(),
            method: crate::api::schema::Method::PaneFocusDirection(
                crate::api::schema::PaneFocusDirectionParams {
                    pane_id: Some("w1:p1".into()),
                    direction: crate::api::schema::PaneDirection::Right,
                },
            ),
        };
        let pane_resize = crate::api::schema::Request {
            id: "req_8".into(),
            method: crate::api::schema::Method::PaneResize(crate::api::schema::PaneResizeParams {
                pane_id: Some("w1:p1".into()),
                direction: crate::api::schema::PaneDirection::Right,
                amount: Some(0.05),
            }),
        };
        let agent_view = crate::api::schema::Request {
            id: "req_9".into(),
            method: crate::api::schema::Method::AgentViewClear(
                crate::api::schema::AgentViewClearParams::default(),
            ),
        };

        assert!(!crate::api::request_changes_ui(&read_only));
        assert!(!crate::api::request_changes_ui(&worktree_list));
        assert!(crate::api::request_changes_ui(&mutating));
        assert!(crate::api::request_changes_ui(&pane_rename));
        assert!(crate::api::request_changes_ui(&worktree_create));
        assert!(crate::api::request_changes_ui(&pane_swap));
        assert!(crate::api::request_changes_ui(&pane_focus_direction));
        assert!(crate::api::request_changes_ui(&pane_resize));
        assert!(crate::api::request_changes_ui(&agent_view));
    }

    #[test]
    fn workspace_create_response_includes_initial_tab_and_root_pane() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("api-root-pane")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace,
            tab,
            root_pane,
        } = app.workspace_created_result(0).unwrap()
        else {
            panic!("expected workspace_created response");
        };

        assert_eq!(workspace.label, "api-root-pane");
        assert_eq!(tab.workspace_id, workspace.workspace_id);
        assert_eq!(root_pane.workspace_id, workspace.workspace_id);
        assert_eq!(root_pane.tab_id, tab.tab_id);
        assert!(root_pane.terminal_id.starts_with("term_"));
        assert_ne!(root_pane.terminal_id, root_pane.pane_id);
    }

    #[test]
    fn tab_create_response_includes_root_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-tab-root-pane");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let crate::api::schema::ResponseResult::TabCreated { tab, root_pane } =
            app.tab_created_result(0, 1).unwrap()
        else {
            panic!("expected tab_created response");
        };

        assert_eq!(tab.workspace_id, root_pane.workspace_id);
        assert_eq!(root_pane.tab_id, tab.tab_id);
        assert_eq!(tab.pane_count, 1);
    }

    #[test]
    fn tab_info_number_uses_stable_public_tab_number() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-tab-public-number");
        let removed_tab = workspace.test_add_tab(None);
        let survivor_tab = workspace.test_add_tab(None);
        let survivor_pane = workspace.tabs[survivor_tab].root_pane;
        assert!(workspace.close_tab(removed_tab));
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let survivor_idx = app.state.workspaces[0]
            .find_tab_index_for_pane(survivor_pane)
            .unwrap();

        let tab = app.tab_info(0, survivor_idx).unwrap();

        assert_eq!(tab.tab_id, format!("{}:t3", app.state.workspaces[0].id));
        assert_eq!(tab.number, 3);
        assert_eq!(tab.label, "2");
    }

    #[test]
    fn legacy_bare_tab_id_uses_tab_position_not_public_tab_number() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("legacy-tab-id");
        let removed_tab = workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        let public_four_tab = workspace.test_add_tab(None);
        let fourth_position_tab = workspace.test_add_tab(None);
        let public_four_pane = workspace.tabs[public_four_tab].root_pane;
        let fourth_position_pane = workspace.tabs[fourth_position_tab].root_pane;
        assert!(workspace.close_tab(removed_tab));
        app.state.workspaces = vec![workspace];

        let public_four_idx = app.state.workspaces[0]
            .find_tab_index_for_pane(public_four_pane)
            .unwrap();
        let fourth_position_idx = app.state.workspaces[0]
            .find_tab_index_for_pane(fourth_position_pane)
            .unwrap();

        assert_eq!(app.state.workspaces[0].tabs[public_four_idx].number, 4);
        assert_eq!(app.state.workspaces[0].tabs[fourth_position_idx].number, 5);
        assert_eq!(
            app.parse_tab_id(&format!("{}:t4", app.state.workspaces[0].id)),
            Some((0, public_four_idx))
        );
        assert_eq!(
            app.parse_tab_id(&format!("{}:4", app.state.workspaces[0].id)),
            Some((0, fourth_position_idx))
        );
    }

    #[test]
    fn workspace_creation_in_navigate_mode_uses_selected_workspace_seed_cwd() {
        let mut app = test_app();
        let mut first = Workspace::test_new("herdr");
        first.identity_cwd = std::path::PathBuf::from("/tmp/herdr");
        let mut second = Workspace::test_new("pion");
        second.identity_cwd = std::path::PathBuf::from("/tmp/pion");

        app.state.workspaces = vec![first, second];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.set_server_mode(Mode::Navigate);

        let ws_idx = app.workspace_creation_source().unwrap();
        let seed_cwd = app.seed_cwd_from_workspace(ws_idx).unwrap();

        assert_eq!(ws_idx, 1);
        assert_eq!(seed_cwd, std::path::PathBuf::from("/tmp/pion"));
    }

    #[test]
    fn new_terminal_cwd_follow_uses_source_cwd() {
        let cwd = creation::resolve_new_terminal_cwd(
            &crate::config::NewTerminalCwdConfig::Follow,
            Some(std::path::PathBuf::from("/tmp/herdr-source")),
        );

        assert_eq!(cwd, std::path::PathBuf::from("/tmp/herdr-source"));
    }

    #[test]
    fn new_terminal_cwd_follow_without_source_uses_home() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };

        let cwd =
            creation::resolve_new_terminal_cwd(&crate::config::NewTerminalCwdConfig::Follow, None);

        assert_eq!(cwd, home);
    }

    #[test]
    fn new_terminal_cwd_path_uses_configured_path() {
        let cwd = creation::resolve_new_terminal_cwd(
            &crate::config::NewTerminalCwdConfig::Path("/tmp/herdr-fixed".into()),
            Some(std::path::PathBuf::from("/tmp/herdr-source")),
        );

        assert_eq!(cwd, std::path::PathBuf::from("/tmp/herdr-fixed"));
    }

    #[test]
    fn server_stop_request_sets_should_quit_flag() {
        let mut app = test_app();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_server_stop".into(),
            method: crate::api::schema::Method::ServerStop(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        assert!(app.state.should_quit);
    }

    #[test]
    fn pane_rename_request_sets_and_clears_manual_label() {
        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-rename");
        let pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let pane_id = app.pane_info(0, pane).unwrap().pane_id;
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_rename".into(),
            method: crate::api::schema::Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id: pane_id.clone(),
                label: Some("reviewer".into()),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(response["result"]["pane"]["label"], "reviewer");
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .manual_label
                .as_deref(),
            Some("reviewer")
        );

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_rename_clear".into(),
            method: crate::api::schema::Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id,
                label: None,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert!(response["result"]["pane"].get("label").is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .unwrap()
            .manual_label
            .is_none());
    }

    #[test]
    fn terminal_and_agent_targets_treat_terminal_ids_differently() {
        let mut app = test_app();
        let workspace = Workspace::test_new("terminal-target-id");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().to_string();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        let resolved = app.resolve_terminal_target(&terminal_id).unwrap();
        assert_eq!(resolved.pane_id, pane);
        assert_eq!(resolved.terminal_id, terminal_id);

        assert!(matches!(
            app.resolve_agent_target(&resolved.terminal_id),
            Err(crate::app::terminal_targets::TerminalTargetError::NotFound { .. })
        ));
    }

    #[test]
    fn agent_target_rejects_a_pane_that_only_has_a_launch_command() {
        let mut app = test_app();
        let workspace = Workspace::test_new("terminal-target-command");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.status_bar_enabled = false;
        app.state.reconcile_sidebar_presentation();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .launch_argv = Some(vec!["just".into(), "dev".into()]);
        let pane_id = app.public_pane_id(0, pane).unwrap();

        assert!(app.resolve_terminal_target(&pane_id).is_ok());
        assert!(matches!(
            app.resolve_agent_target(&pane_id),
            Err(crate::app::terminal_targets::TerminalTargetError::NotFound { .. })
        ));
    }

    #[test]
    fn terminal_target_resolves_pane_id_for_an_agent() {
        let mut app = test_app();
        let workspace = Workspace::test_new("terminal-target-pane");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().to_string();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let attached_terminal_id = app.state.workspaces[0].terminal_id(pane).cloned().unwrap();
        app.state
            .terminals
            .get_mut(&attached_terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Idle,
            );
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.public_pane_id(0, pane).unwrap();

        let resolved = app.resolve_terminal_target(&pane_id).unwrap();

        assert_eq!(resolved.pane_id, pane);
        assert_eq!(resolved.terminal_id, terminal_id);
    }

    #[test]
    fn terminal_target_resolves_unique_agent_name() {
        let mut app = test_app();
        let workspace = Workspace::test_new("terminal-target-name");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().to_string();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let attached_terminal_id = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&attached_terminal_id)
            .unwrap()
            .set_agent_name("reviewer".into());
        app.state.active = Some(0);
        app.state.selected = 0;

        let resolved = app.resolve_terminal_target("reviewer").unwrap();

        assert_eq!(resolved.pane_id, pane);
        assert_eq!(resolved.terminal_id, terminal_id);
    }

    #[test]
    fn agent_target_treats_legacy_pane_syntax_as_a_name() {
        let mut app = test_app();
        let workspace = Workspace::test_new("agent-target-name");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        terminal.set_agent_name("p_1".into());

        let resolved = app.resolve_agent_target("p_1").unwrap();

        assert_eq!(resolved.pane_id, pane);
        assert_eq!(resolved.terminal_id, terminal_id.to_string());
    }

    #[test]
    fn terminal_target_reports_missing_target() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("terminal-target-missing")];
        app.state.active = Some(0);
        app.state.selected = 0;

        let err = app.resolve_terminal_target("missing-agent").unwrap_err();

        assert_eq!(
            err,
            crate::app::terminal_targets::TerminalTargetError::NotFound {
                target: "missing-agent".into()
            }
        );
    }

    #[test]
    fn terminal_target_reports_ambiguous_duplicate_agent_name() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("terminal-target-ambiguous");
        let first = workspace.tabs[0].root_pane;
        let second = workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0]
            .pane_state(first)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("worker".into());
        let second_terminal_id = app.state.workspaces[0]
            .pane_state(second)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .set_agent_name("worker".into());
        app.state.active = Some(0);
        app.state.selected = 0;

        let err = app.resolve_terminal_target("worker").unwrap_err();

        let crate::app::terminal_targets::TerminalTargetError::Ambiguous { target, candidates } =
            err
        else {
            panic!("expected ambiguous terminal target");
        };
        assert_eq!(target, "worker");
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| {
            candidate.terminal_id.starts_with("term_")
                && candidate.pane_id.starts_with(&app.state.workspaces[0].id)
                && candidate.workspace_id == app.state.workspaces[0].id
                && candidate.cwd.is_some()
        }));
    }

    #[tokio::test]
    async fn pane_split_request_targets_pane_in_background_tab() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let original_shell = std::env::var_os("SHELL");
        env.set("SHELL", exiting_test_command());

        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-pane-split-background-tab");
        let active_pane = workspace.tabs[0].root_pane;
        let background_tab = workspace.test_add_tab(Some("worker"));
        let target_pane = workspace.tabs[background_tab].root_pane;
        workspace.switch_tab(background_tab);
        let background_previous_focus =
            workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.switch_tab(0);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let split_cwd = std::env::temp_dir();
        let target_terminal_id = app.state.workspaces[0]
            .pane_state(target_pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&target_terminal_id)
            .unwrap()
            .cwd = split_cwd.clone();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state
            .focus_pane_in_workspace(0, background_previous_focus);
        app.state.focus_pane_in_workspace(0, active_pane);

        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;
        let target_tab_id = app.public_tab_id(0, background_tab).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_background_tab".into(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some(target_pane_id),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: false,
                right_click: Default::default(),
                env: Default::default(),
                work_context: None,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(response["result"]["pane"]["tab_id"], target_tab_id);
        let response_cwd =
            std::path::PathBuf::from(response["result"]["pane"]["cwd"].as_str().unwrap());
        assert_eq!(
            crate::worktree::canonical_or_original(&response_cwd),
            crate::worktree::canonical_or_original(&split_cwd)
        );
        assert_eq!(response["result"]["pane"]["focused"], false);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            active_pane
        );
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[background_tab]
                .layout
                .focused(),
            background_previous_focus
        );
        assert_eq!(
            app.state.workspaces[0].tabs[background_tab]
                .layout
                .pane_count(),
            3
        );
        app.state.last_pane();
        assert_eq!(app.state.workspaces[0].active_tab, background_tab);
        assert_eq!(
            app.state.workspaces[0].tabs[background_tab]
                .layout
                .focused(),
            background_previous_focus
        );

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => env.set("SHELL", value),
            None => env.remove("SHELL"),
        }
    }

    #[tokio::test]
    async fn pane_split_companion_request_focuses_new_pane_when_requested() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let original_shell = std::env::var_os("SHELL");
        env.set("SHELL", exiting_test_command());

        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-pane-split-focus-background-tab");
        let background_tab = workspace.test_add_tab(Some("worker"));
        workspace.switch_tab(0);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let target_pane = app.state.workspaces[0].tabs[background_tab].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;
        let target_tab_id = app.public_tab_id(0, background_tab).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_focus_background_tab".into(),
            method: crate::api::schema::Method::PaneSplitCompanion(
                crate::api::schema::PaneSplitParams {
                    workspace_id: None,
                    target_pane_id: Some(target_pane_id),
                    direction: crate::api::schema::SplitDirection::Right,
                    ratio: None,
                    cwd: None,
                    focus: true,
                    right_click: Default::default(),
                    env: Default::default(),
                    work_context: None,
                },
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(response["result"]["pane"]["tab_id"], target_tab_id);
        assert_eq!(response["result"]["pane"]["focused"], true);
        let created_pane_id = app
            .parse_pane_id(response["result"]["pane"]["pane_id"].as_str().unwrap())
            .unwrap()
            .1;
        assert!(
            app.state.workspaces[0]
                .pane_state(created_pane_id)
                .unwrap()
                .is_companion
        );
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab, background_tab);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => env.set("SHELL", value),
            None => env.remove("SHELL"),
        }
    }

    #[tokio::test]
    async fn pane_split_request_applies_ratio() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let original_shell = std::env::var_os("SHELL");
        env.set("SHELL", "/usr/bin/true");

        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-split-ratio");
        let target_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_ratio".into(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some(target_pane_id),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: Some(0.333),
                cwd: None,
                focus: false,
                right_click: crate::api::schema::PaneRightClickTarget::Pane,
                env: Default::default(),
                work_context: None,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        let splits = app.state.workspaces[0].tabs[0]
            .layout
            .splits(ratatui::layout::Rect::new(0, 0, 100, 20));
        assert_eq!(splits.len(), 1);
        assert!((splits[0].ratio - 0.333).abs() < f32::EPSILON);
        let response_pane_id = response["result"]["pane"]["pane_id"].as_str().unwrap();
        let (_, response_pane_id) = app.parse_pane_id(response_pane_id).unwrap();
        assert!(
            !app.state.workspaces[0]
                .pane_state(response_pane_id)
                .unwrap()
                .is_companion
        );
        assert!(
            app.state.workspaces[0]
                .pane_state(response_pane_id)
                .unwrap()
                .right_click_passthrough
        );

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => env.set("SHELL", value),
            None => env.remove("SHELL"),
        }
    }

    #[tokio::test]
    async fn pane_split_request_uses_active_focused_pane_when_target_is_omitted() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let original_shell = std::env::var_os("SHELL");
        env.set("SHELL", "/usr/bin/true");

        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-split-current");
        let target_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.focus_pane_in_workspace(0, target_pane);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_current".into(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: false,
                right_click: Default::default(),
                env: Default::default(),
                work_context: None,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            target_pane
        );

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => env.set("SHELL", value),
            None => env.remove("SHELL"),
        }
    }

    #[tokio::test]
    async fn unavailable_agent_start_does_not_mutate_topology() {
        let mut app = test_app();
        let workspace = Workspace::test_new("agent-start-target");
        let root = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.pane_info(0, root).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_agent_start_target".into(),
            method: crate::api::schema::Method::AgentStart(crate::api::schema::AgentStartParams {
                name: "worker".into(),
                kind: "pi".into(),
                pane_id,
                args: Vec::new(),
                timeout_ms: Some(1_000),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["error"]["code"], "agent_pane_unavailable");
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
    }

    #[tokio::test]
    async fn failed_agent_start_input_rolls_back_and_can_retry() {
        let mut app = test_app();
        let workspace = Workspace::test_new("agent-start-input-failure");
        let root = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.pane_info(0, root).unwrap().pane_id;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("shell".into());
        let (runtime, mut receiver) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
        runtime
            .try_send_bytes(bytes::Bytes::from_static(b"occupied"))
            .unwrap();
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let request = || crate::api::schema::Request {
            id: "req_agent_start_input".into(),
            method: crate::api::schema::Method::AgentStart(crate::api::schema::AgentStartParams {
                name: "worker".into(),
                kind: "pi".into(),
                pane_id: pane_id.clone(),
                args: Vec::new(),
                timeout_ms: Some(4_000),
            }),
        };
        let response = app.handle_api_request(request());
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["error"]["code"], "agent_start_input_failed");
        assert_eq!(app.state.terminals[&terminal_id].agent_name, None);
        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("shell")
        );

        assert_eq!(
            receiver.try_recv().unwrap(),
            bytes::Bytes::from_static(b"occupied")
        );
        let retry = app.handle_api_request(request());
        let retry: serde_json::Value = serde_json::from_str(&retry).unwrap();
        assert_eq!(retry["result"]["type"], "agent_started");
        assert_eq!(
            app.state.terminals[&terminal_id].agent_name.as_deref(),
            Some("worker")
        );
        let rename = app.handle_api_request(crate::api::schema::Request {
            id: "req_agent_rename_pending".into(),
            method: crate::api::schema::Method::AgentRename(
                crate::api::schema::AgentRenameParams {
                    target: pane_id,
                    name: Some("replacement".into()),
                },
            ),
        });
        let rename: serde_json::Value = serde_json::from_str(&rename).unwrap();
        assert_eq!(rename["error"]["code"], "agent_launch_pending");
        assert_eq!(
            app.state.terminals[&terminal_id].agent_name.as_deref(),
            Some("worker")
        );
    }

    #[test]
    fn pane_close_request_closes_only_the_target_tab_when_other_tabs_exist() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-pane-close");
        let second_tab = workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(second_tab);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let target_pane = app.state.workspaces[0].tabs[second_tab].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_close".into(),
            method: crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: target_pane_id,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "api-pane-close");
    }

    #[tokio::test]
    async fn pane_close_request_closes_workspace_with_final_tab() {
        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-close-last");
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let target_pane = app.state.workspaces[0].tabs[0].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_close_last".into(),
            method: crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: target_pane_id,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        assert!(app.state.workspaces.is_empty());
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
        for (_terminal_id, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn pane_close_request_keeps_parent_worktree_group_open() {
        let mut app = test_app();
        let mut parent = Workspace::test_new("api-pane-close-parent");
        parent.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        let mut child = Workspace::test_new("api-pane-close-child");
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-child".into(),
            is_linked_worktree: true,
        });
        app.state.workspaces = vec![parent, child];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 1;

        let target_pane = app.state.workspaces[0].tabs[0].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_close_parent_group".into(),
            method: crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: target_pane_id,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["error"]["code"], "confirmation_required");
        assert_ne!(
            app.state.client_overlay,
            crate::app::state::ClientOverlay::ConfirmClose
        );
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs[0].root_pane, target_pane);
        assert!(app.state.workspaces[0].worktree_space.is_some());
        assert!(!app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
        for (_terminal_id, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[test]
    fn session_dirty_flag_schedules_debounced_save() {
        let mut app = test_app();
        app.no_session = false;
        app.state.mark_session_dirty();

        app.sync_session_save_schedule();

        assert!(app.state.session_dirty);
        assert!(app.session_save_deadline.is_some());
    }

    #[test]
    fn ac1_5_second_dirty_mutation_resets_debounce_deadline() {
        let mut app = test_app();
        app.no_session = false;
        app.state.mark_session_dirty();
        app.sync_session_save_schedule();
        let first_deadline = app.session_save_deadline.expect("first deadline");

        std::thread::sleep(Duration::from_millis(2));
        app.state.mark_session_dirty();
        app.sync_session_save_schedule();

        assert!(app.session_save_deadline.expect("second deadline") > first_deadline);
    }

    #[test]
    fn ac1_5_explicit_save_schedule_records_dirty_revision() {
        let mut app = test_app();
        app.no_session = false;
        let previous_revision = app.state.session_dirty_revision;

        app.schedule_session_save();

        assert!(app.state.session_dirty);
        assert_eq!(
            app.state.session_dirty_revision,
            previous_revision.wrapping_add(1)
        );
    }

    #[test]
    fn ac1_5_dirty_mutation_does_not_disturb_retry_deadline() {
        let mut app = test_app();
        app.no_session = false;
        let retry_deadline = Instant::now() + Duration::from_secs(3);
        app.session_save_retry_deadline = Some(retry_deadline);
        app.session_save_deadline = Some(retry_deadline);

        let previous_revision = app.state.session_dirty_revision;
        app.schedule_session_save();

        assert_eq!(
            app.state.session_dirty_revision,
            previous_revision.wrapping_add(1)
        );
        assert_eq!(app.session_save_retry_deadline, Some(retry_deadline));
        assert_eq!(app.session_save_deadline, Some(retry_deadline));
    }

    #[test]
    fn next_loop_deadline_includes_session_save_deadline() {
        let mut app = test_app();
        let now = Instant::now();
        app.session_save_deadline = Some(now + Duration::from_secs(2));
        app.next_resize_poll = now + Duration::from_secs(5);
        app.next_auto_update_check = Some(now + Duration::from_secs(6));

        assert_eq!(
            app.next_loop_deadline(now, false),
            app.session_save_deadline
        );
    }

    #[test]
    fn activity_age_refresh_is_scheduled_for_space_suffix_rows() {
        let mut app = test_app();
        let started = Instant::now();
        let workspace = Workspace::test_new("activity");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.notepad.set_visible_tabs(Vec::new());
        app.state.reconcile_sidebar_presentation();
        app.state.ensure_test_terminals();
        app.state
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

        let observed = started + Duration::from_secs(7);
        // F19-1a keeps the visible age before the optional Space suffix, so its
        // minute boundary must keep waking the render loop.
        app.state.sidebar_width = app.state.sidebar_max_width;
        crate::ui::compute_view_with_runtime_registry(
            &mut app.state,
            &app.terminal_runtimes,
            // Keep this characterization in a wide viewport rather than testing
            // the separately-covered narrow-row age elision.
            ratatui::layout::Rect::new(0, 0, 160, 20),
        );
        app.sync_agent_activity_refresh_deadline(observed);

        assert_eq!(
            app.agent_activity_refresh_deadline,
            Some(started + Duration::from_secs(60))
        );
        assert!(!app.take_due_agent_activity_refresh(observed));
        assert!(app.take_due_agent_activity_refresh(started + Duration::from_secs(60)));
        assert!(app.agent_activity_refresh_deadline.is_none());
    }

    #[test]
    fn headless_activity_refresh_deadline_requires_an_attached_client() {
        let mut app = test_app();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(2);
        app.next_resize_poll = now + Duration::from_secs(1);
        app.config_diagnostic_deadline = None;
        app.toast_deadline = None;
        app.next_auto_update_check = None;
        app.next_agent_manifest_update_check = None;
        app.agent_metadata_deadline = None;
        app.pending_agent_resume_deadline = None;
        app.session_save_deadline = None;
        app.selection_autoscroll_deadline = None;
        app.selection_highlight_clear_deadline = None;
        app.state.status_bar_enabled = false;
        app.state.status_metrics = None;
        app.state.workspaces.clear();
        app.agent_activity_refresh_deadline = Some(deadline);

        assert_eq!(
            app.next_headless_loop_deadline_with_client_refresh(now, false, true),
            Some(deadline)
        );
        assert_eq!(
            app.next_headless_loop_deadline_with_client_refresh(now, false, false),
            None
        );
    }

    #[test]
    fn headless_next_loop_deadline_ignores_resize_poll() {
        let mut app = test_app();
        let now = Instant::now();
        app.next_resize_poll = now + Duration::from_millis(100);
        app.session_save_deadline = Some(now + Duration::from_secs(2));
        app.next_auto_update_check = Some(now + Duration::from_secs(6));

        assert_eq!(
            app.next_headless_loop_deadline_with_client_refresh(now, false, true),
            app.session_save_deadline
        );
    }

    #[test]
    fn headless_next_loop_deadline_returns_none_when_resize_poll_is_only_deadline() {
        let mut app = test_app();
        let now = Instant::now();
        app.next_resize_poll = now - Duration::from_millis(1);
        app.config_diagnostic_deadline = None;
        app.toast_deadline = None;
        app.next_auto_update_check = None;
        app.session_save_deadline = None;
        app.state.workspaces.clear();

        assert_eq!(
            app.next_headless_loop_deadline_with_client_refresh(now, false, true),
            None
        );
    }

    #[test]
    fn due_session_save_deadline_is_cleared() {
        let mut app = test_app();
        app.session_save_deadline = Some(Instant::now() - Duration::from_secs(1));

        app.handle_scheduled_tasks(Instant::now(), false);

        assert!(app.session_save_deadline.is_none());
    }

    #[test]
    fn due_session_save_starts_background_writer() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let config_home = unique_temp_path("background-session-save");
        env.set("XDG_CONFIG_HOME", &config_home);
        env.remove(crate::session::SESSION_ENV_VAR);

        let mut app = test_app();
        app.no_session = false;
        app.state.workspaces = vec![Workspace::test_new("autosave")];
        app.state.ensure_test_terminals();
        app.state.session_dirty = true;
        app.session_save_deadline = Some(Instant::now() - Duration::from_secs(1));

        app.handle_scheduled_tasks(Instant::now(), false);

        assert!(app.session_save_thread.is_some());
        assert!(app.session_save_deadline.is_some());
        app.save_session_now();
        assert!(crate::session::data_dir().join("session.json").exists());

        env.remove("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(config_home);
    }

    #[test]
    fn background_session_save_reschedules_when_writer_is_busy() {
        let mut app = test_app();
        app.no_session = false;
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        app.session_save_thread = Some(std::thread::spawn(move || {
            let _ = release_rx.recv();
            session::SessionSaveResult {
                revision: 0,
                result: Ok(()),
            }
        }));

        app.start_background_session_save();

        assert!(app.session_save_thread.is_some());
        assert!(app.session_save_deadline.is_some());

        release_tx.send(()).unwrap();
        app.no_session = true;
        app.save_session_now();
    }

    #[test]
    fn ac1_applied_save_failure_keeps_dirty_and_preserves_retry_through_sync() {
        let mut app = test_app();
        app.no_session = false;
        app.state.session_dirty = true;
        app.state.session_dirty_revision = 7;
        app.apply_session_save_result(session::SessionSaveResult {
            revision: 7,
            result: Err(std::io::Error::other("injected save failure")),
        });
        let retry_deadline = app.session_save_deadline.expect("retry deadline");

        app.sync_session_save_schedule();

        assert!(app.state.session_dirty);
        assert_eq!(app.session_save_failures, 1);
        assert_eq!(app.session_save_deadline, Some(retry_deadline));
    }

    #[test]
    fn session_save_retries_after_a_stalled_writer_releases() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let config_home = unique_temp_path("recovered-background-session-save");
        env.set("XDG_CONFIG_HOME", &config_home);
        env.remove(crate::session::SESSION_ENV_VAR);

        let mut app = test_app();
        app.no_session = false;
        app.state.workspaces = vec![Workspace::test_new("recovered-autosave")];
        app.state.ensure_test_terminals();
        app.state.mark_session_dirty();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let revision = app.state.session_dirty_revision;
        app.session_save_thread = Some(std::thread::spawn(move || {
            let _ = release_rx.recv();
            session::SessionSaveResult {
                revision,
                result: Err(std::io::Error::other("injected stalled writer failure")),
            }
        }));
        let now = Instant::now();
        app.session_dirty_since = Some(now - Duration::from_secs(60));
        app.sync_session_save_schedule();
        assert!(app.session_save_thread.is_some());
        assert!(app.session_save_stall_warning_at.is_some());

        release_tx.send(()).unwrap();
        while app
            .session_save_thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            std::thread::yield_now();
        }
        app.reap_finished_session_save();
        assert!(app.state.session_dirty);
        assert!(app.session_save_retry_deadline.is_some());

        std::fs::create_dir_all(&config_home).unwrap();
        app.session_save_retry_deadline = Some(Instant::now() - Duration::from_millis(1));
        app.session_save_deadline = app.session_save_retry_deadline;
        app.start_background_session_save();
        app.save_session_now();

        assert!(crate::session::data_dir().join("session.json").exists());
        assert!(!app.state.session_dirty);
        assert_eq!(app.session_save_failures, 0);

        drop(env);
        let _ = std::fs::remove_dir_all(config_home);
    }

    #[test]
    fn ac1_4_real_persist_failure_is_reaped_and_schedules_bounded_retry() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let config_home = unique_temp_path("failed-background-session-save");
        std::fs::write(&config_home, b"not a directory").unwrap();
        env.set("XDG_CONFIG_HOME", &config_home);
        env.remove(crate::session::SESSION_ENV_VAR);

        let mut app = test_app();
        app.no_session = false;
        app.state.workspaces = vec![Workspace::test_new("failed-autosave")];
        app.state.ensure_test_terminals();
        app.state.mark_session_dirty();
        let save_started = Instant::now();

        app.start_background_session_save();
        let writer_spawned = app.session_save_thread.is_some();
        while app
            .session_save_thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            std::thread::yield_now();
        }
        app.sync_session_save_schedule();
        let reaped_at = Instant::now();
        let retry_deadline = app.session_save_retry_deadline;

        drop(env);
        std::fs::remove_file(config_home).unwrap();

        assert!(writer_spawned);
        assert!(app.session_save_thread.is_none());
        assert!(app.state.session_dirty);
        assert_eq!(app.session_save_failures, 1);
        assert_eq!(app.session_save_deadline, retry_deadline);
        assert!(retry_deadline.is_some_and(|deadline| {
            deadline >= save_started + Duration::from_millis(250)
                && deadline <= reaped_at + Duration::from_millis(250)
        }));
    }

    #[test]
    fn ac1_success_does_not_clear_mutation_made_during_save_and_resets_backoff() {
        let mut app = test_app();
        app.no_session = false;
        app.state.session_dirty = true;
        app.state.session_dirty_revision = 11;
        app.session_save_failures = 4;

        app.apply_session_save_result(session::SessionSaveResult {
            revision: 10,
            result: Ok(()),
        });
        app.sync_session_save_schedule();

        assert!(app.state.session_dirty);
        assert_eq!(app.session_save_failures, 0);
        assert!(app.session_save_deadline.is_some());
    }

    #[test]
    fn final_session_save_joins_background_writer_before_returning() {
        let mut app = test_app();
        app.no_session = true;
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        app.session_save_thread = Some(std::thread::spawn(move || {
            let _ = release_rx.recv();
            done_tx.send(()).unwrap();
            session::SessionSaveResult {
                revision: 0,
                result: Ok(()),
            }
        }));
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            release_tx.send(()).unwrap();
        });

        app.save_session_now();

        releaser.join().unwrap();
        done_rx.try_recv().unwrap();
        assert!(app.session_save_thread.is_none());
    }

    #[test]
    fn next_loop_deadline_includes_selection_autoscroll_deadline() {
        let mut app = test_app();
        let now = Instant::now();
        app.next_resize_poll = now + Duration::from_millis(300);
        app.selection_autoscroll_deadline = Some(now + Duration::from_millis(5));
        app.session_save_deadline = Some(now + Duration::from_millis(200));
        assert_eq!(
            app.next_loop_deadline(now, false),
            app.selection_autoscroll_deadline
        );
    }

    #[test]
    fn tick_selection_autoscroll_self_heals_when_state_cleared() {
        let mut app = test_app();
        let now = Instant::now();
        app.state.selection_autoscroll = None;
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_on_rect_change() {
        let mut app = test_app();
        let now = Instant::now();
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.selection = Some(crate::selection::Selection::anchor(pane_id, 0, 0, None));
        // Set autoscroll with a stale inner_rect that doesn't match pane_infos
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 999,
            inner_rect: ratatui::layout::Rect::new(0, 0, 1, 1), // wrong rect
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn full_internal_event_queue_eventually_applies_working_to_idle_transition() {
        let mut app = test_app();
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.handle_internal_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .raw_agent_state(),
            AgentState::Working
        );

        for i in 0..APP_EVENT_CHANNEL_CAPACITY {
            app.event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("9.9.{i}"),
                    install_command: "herdr update".into(),
                })
                .unwrap();
        }

        let tx = app.event_tx.clone();
        let send = tx.send(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        tokio::pin!(send);

        let blocked =
            tokio::time::timeout(Duration::from_millis(20), async { (&mut send).await }).await;
        assert!(
            blocked.is_err(),
            "state change sender should wait for queue space instead of failing"
        );

        app.drain_internal_events();

        tokio::time::timeout(Duration::from_millis(50), async { (&mut send).await })
            .await
            .expect("state change should enqueue once queue space is available")
            .expect("app event receiver should still be alive");

        let max_drains = (APP_EVENT_CHANNEL_CAPACITY / APP_EVENT_DRAIN_LIMIT) + 2;
        for _ in 0..max_drains {
            if app
                .state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .raw_agent_state()
                == AgentState::Idle
            {
                break;
            }
            app.drain_internal_events();
        }

        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .raw_agent_state(),
            AgentState::Idle,
            "Working→Idle should still apply after temporary queue pressure"
        );
    }

    #[test]
    fn route_client_input_dispatches_navigate_mode_keybinds() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;

        // Start in navigate mode.
        app.state.set_server_mode(Mode::Navigate);

        // Send Ctrl+B then Esc (prefix → leave navigate mode).
        // Ctrl+B is 0x02 in raw terminal input.
        // After entering navigate mode and pressing Esc, we should leave navigate mode.
        let esc_bytes = vec![0x1b]; // Esc
        app.route_client_input(esc_bytes);
        // Esc in navigate mode should leave navigate mode.
        assert_eq!(
            app.state.server_mode(),
            Mode::Terminal,
            "Esc should leave navigate mode and return to Terminal mode"
        );
    }

    #[test]
    fn explicit_text_commit_does_not_trigger_navigate_binding() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.detach_exits = false;
        app.state.set_server_mode(Mode::Navigate);

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Text(
                crate::input::TextCommit::new("q"),
            )],
            false,
        );

        assert!(!app.state.detach_requested);
        assert_eq!(app.state.server_mode(), Mode::Navigate);
        assert!(app.input_leases.is_empty());
    }

    #[test]
    fn route_client_input_q_detaches_in_persistence_mode() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.detach_exits = false;

        // Start in navigate mode.
        app.state.set_server_mode(Mode::Navigate);
        assert!(!app.state.detach_requested);

        let q_bytes = b"q".to_vec();
        app.route_client_input(q_bytes);

        assert!(
            app.state.detach_requested,
            "q should detach in persistence mode"
        );
        assert_eq!(
            app.state.server_mode(),
            Mode::Terminal,
            "q should leave navigate mode"
        );
    }

    #[test]
    fn route_client_input_prefix_then_q_detaches_in_persistence_mode() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.detach_exits = false;

        // Start in terminal mode (default after workspace creation).
        app.state.set_server_mode(Mode::Terminal);
        assert!(!app.state.detach_requested);

        // Send Ctrl+B (prefix key, raw byte 0x02).
        let prefix_bytes = vec![0x02];
        app.route_client_input(prefix_bytes);

        assert_eq!(
            app.state.server_mode(),
            Mode::Prefix,
            "prefix key should enter prefix mode"
        );
        assert!(
            !app.state.detach_requested,
            "prefix key should not set detach flag"
        );

        let q_bytes = b"q".to_vec();
        app.route_client_input(q_bytes);

        assert!(
            app.state.detach_requested,
            "q should detach in persistence mode"
        );
        assert_eq!(
            app.state.server_mode(),
            Mode::Terminal,
            "q should leave navigate mode"
        );
    }

    #[test]
    fn route_client_input_prefix_tab_dispatches_global_last_pane() {
        let config: Config = toml::from_str(
            r#"
[keys]
last_pane = "prefix+tab"
"#,
        )
        .unwrap();
        let mut app = test_app();
        let mut first = Workspace::test_new("one");
        let first_second_tab = first.test_add_tab(Some("logs"));
        let first_second_root = first.tabs[first_second_tab].root_pane;
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;
        app.state.workspaces = vec![first, second];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.keybinds = config.keybinds();
        app.state.set_server_mode(Mode::Terminal);
        app.state.switch_workspace_tab(0, first_second_tab);
        app.state.switch_workspace_tab(1, 0);

        app.route_client_input(vec![0x02, b'\t']);

        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab, first_second_tab);
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(first_second_root)
        );

        app.route_client_input(vec![0x02, b'\t']);

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(second_root));
    }

    #[tokio::test]
    async fn pre_render_status_context_matches_deferred_new_tab_click() {
        let mut app = test_app();
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
        app.state.new_terminal_cwd = crate::config::NewTerminalCwdConfig::Path(
            std::env::temp_dir().to_string_lossy().into_owned(),
        );
        app.state.workspaces = vec![Workspace::test_new("new-tab-status")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        // This test clicks the new-tab button, so it needs a tab row rather
        // than the shipped `Hidden` default.
        app.state.tab_bar_position = crate::config::TabBarPositionConfig::Top;
        app.state.prompt_new_tab_name = false;
        app.state.ensure_test_terminals();
        let previous_pane = app.state.workspaces[0].tabs[0].root_pane;
        let previous_terminal = app.state.workspaces[0]
            .terminal_id(previous_pane)
            .expect("previous terminal")
            .clone();
        app.state.terminals.get_mut(&previous_terminal).unwrap().cwd = "/old-focus".into();
        app.state.sync_status_focused_cached_cwd();
        app.state.status_git_cwd = Some("/old-focus".into());
        app.state.status_git_branch = Some("old-branch".into());
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let new_tab_area = app.state.view.new_tab_hit_area;

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: new_tab_area.x + 1,
                row: new_tab_area.y,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;
        assert!(app.state.request_new_tab);
        app.state.request_new_tab = false;
        let label = app.state.requested_new_tab_name.take();
        app.runtime_tab_create(
            "test.tab.create",
            crate::api::schema::TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: true,
                label,
                env: Default::default(),
                work_context: None,
            },
        );

        let active_tab = app.state.workspaces[0].active_tab;
        assert_eq!(active_tab, 1);
        let active_pane = app.state.workspaces[0].tabs[active_tab].root_pane;
        let active_terminal = app.state.workspaces[0]
            .terminal_id(active_pane)
            .expect("active terminal");
        let expected_cwd = app.state.terminals[active_terminal].cwd.clone();
        let (stale_cwd, stale_branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(stale_cwd, Some(std::path::PathBuf::from("/old-focus")));
        assert_eq!(stale_branch.as_deref(), Some("old-branch"));

        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();
        assert!(app.sync_status_context_before_render());
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(expected_cwd));
        assert_eq!(branch, None);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);

        api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn pre_render_status_context_preserves_bounded_refresh_projection() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("runtime-status")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.ensure_test_terminals();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.workspaces[0]
            .terminal_id(pane)
            .expect("focused terminal")
            .clone();
        app.state.terminals.get_mut(&terminal).unwrap().cwd = "/cached".into();
        assert!(app.sync_status_context_before_render());

        app.state.status_focused_cwd = Some("/runtime".into());
        app.state.status_git_cwd = Some("/runtime".into());
        app.state.status_git_branch = Some("runtime-branch".into());

        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();
        assert!(!app.sync_status_context_before_render());
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/runtime")));
        assert_eq!(branch.as_deref(), Some("runtime-branch"));
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
    }

    #[test]
    fn status_context_disabled_git_result_does_not_claim_new_focus() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("disabled-git-status");
        let first_tab = workspace.active_tab;
        let first_pane = workspace.tabs[first_tab].root_pane;
        let second_tab = workspace.test_add_tab(Some("second"));
        let second_pane = workspace.tabs[second_tab].root_pane;
        workspace.switch_tab(first_tab);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.ensure_test_terminals();
        let first_terminal = app.state.workspaces[0]
            .terminal_id(first_pane)
            .expect("first terminal")
            .clone();
        let second_terminal = app.state.workspaces[0]
            .terminal_id(second_pane)
            .expect("second terminal")
            .clone();
        app.state.terminals.get_mut(&first_terminal).unwrap().cwd = "/first".into();
        app.state.terminals.get_mut(&second_terminal).unwrap().cwd = "/second".into();
        assert!(app.sync_status_context_before_render());
        app.state.status_git_cwd = Some("/first".into());
        app.state.status_git_branch = Some("first-branch".into());
        let first_focus = app.status_context_focus.clone();

        app.state.status_bar_enabled = false;
        app.state.workspaces[0].switch_tab(second_tab);
        app.test_begin_git_refresh(1);
        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: Vec::new(),
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert_eq!(app.status_context_focus, first_focus);
        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();
        assert!(app.sync_status_context_before_render());
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/second")));
        assert_eq!(branch, None);
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
    }

    #[test]
    fn status_context_inactive_tab_reorder_preserves_runtime_projection() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("tab-reorder-status");
        workspace.test_add_tab(Some("middle"));
        let active_tab = workspace.test_add_tab(Some("active"));
        workspace.switch_tab(active_tab);
        let focused_pane = workspace.tabs[active_tab].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.ensure_test_terminals();
        let terminal = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .expect("focused terminal")
            .clone();
        app.state.terminals.get_mut(&terminal).unwrap().cwd = "/cached".into();
        assert!(app.sync_status_context_before_render());
        app.state.status_focused_cwd = Some("/runtime".into());
        app.state.status_git_cwd = Some("/runtime".into());
        app.state.status_git_branch = Some("runtime-branch".into());
        let projected_focus = app.status_context_focus.clone();
        let previous_active_tab = app.state.workspaces[0].active_tab;

        assert!(app.state.workspaces[0].move_tab(0, 3));
        assert_ne!(app.state.workspaces[0].active_tab, previous_active_tab);
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(focused_pane)
        );
        assert_eq!(app.focused_status_context_key(), projected_focus);

        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();
        assert!(!app.sync_status_context_before_render());
        let (cwd, branch) = crate::ui::focused_status_context_for_test(&app.state);
        assert_eq!(cwd, Some(std::path::PathBuf::from("/runtime")));
        assert_eq!(branch.as_deref(), Some("runtime-branch"));
        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
    }

    #[tokio::test]
    async fn route_client_input_double_prefix_passes_prefix_through_to_focused_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.prefix_code = KeyCode::Char('l');
        app.state.prefix_mods = KeyModifiers::CONTROL;

        app.route_client_input(vec![0x0c]);
        assert_eq!(app.state.server_mode(), Mode::Prefix);

        app.route_client_input(vec![0x0c]);
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from(vec![0x0c]));
    }

    #[tokio::test]
    async fn route_client_input_reencodes_terminal_keys_for_focused_pane_protocol() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        // Ghostty/kitty-style Ctrl-C should be normalized back to the pane's
        // negotiated encoding instead of being forwarded verbatim.
        app.route_client_input(b"\x1b[99;5u".to_vec());

        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from(vec![3]));

        // iTerm2 and rxvt-style hosts may send F4 as CSI 14~. Normalize it
        // through the same semantic key path instead of leaking host bytes.
        app.route_client_input(b"\x1b[14~".to_vec());

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1bOS")
        );
    }

    #[tokio::test]
    async fn host_report_all_supplies_printable_releases_for_event_type_only_panes() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[>4;2m\x1b[=3;1u",
            4,
        );
        assert_eq!(
            runtime.keyboard_protocol(),
            crate::input::KeyboardProtocol::Kitty { flags: 3 }
        );
        assert!(runtime
            .input_state()
            .is_some_and(|state| state.modify_other_keys));
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        assert!(app.host_keyboard_report_all_requested());

        app.route_client_input(b"\x1b[106;1:1u\x1b[106;1:2u\x1b[106;1:3u".to_vec());
        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from_static(b"j"));
        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from_static(b"j"));
        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(rx.try_recv().is_err());

        let runtime = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, focused)
            .unwrap();
        runtime.test_process_pty_bytes(b"\x1b[>4;0m");
        assert!(!runtime
            .input_state()
            .is_some_and(|state| state.modify_other_keys));
        assert!(!app.host_keyboard_report_all_requested());

        #[cfg(unix)]
        {
            runtime.test_process_pty_bytes(b"\x1b[>4;1m");
            assert!(!runtime
                .input_state()
                .is_some_and(|state| state.modify_other_keys));
            assert!(!app.host_keyboard_report_all_requested());
        }
    }

    #[tokio::test]
    async fn host_report_all_follows_terminal_protocol_and_command_modes() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, _rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 1);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        assert!(app.host_keyboard_report_all_requested());

        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, focused)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[<u");
        assert!(!app.host_keyboard_report_all_requested());

        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, focused)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[>15u");
        let other_pane = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let (other_runtime, _rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>7u", 1);
        app.state.workspaces[0].tabs[0]
            .runtimes
            .insert(other_pane, other_runtime);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(other_pane));
        assert!(!app.host_keyboard_report_all_requested());

        assert!(app.state.focus_pane_in_workspace(0, focused));
        app.state.set_server_mode(Mode::Prefix);
        assert!(app.host_keyboard_report_all_requested());
        app.state.set_server_mode(Mode::Navigate);
        assert!(app.host_keyboard_report_all_requested());
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
        assert!(!app.host_keyboard_report_all_requested());
    }

    #[tokio::test]
    async fn route_client_input_forwards_report_all_printable_event_kinds() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 4);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"\x1b[106u\x1b[106;1:2u\x1b[106;1:3u".to_vec());

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:2u")
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn explicit_text_commit_bypasses_bindings_leases_and_key_encoding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 2);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Text(
                crate::input::TextCommit::new("你🙂"),
            )],
            false,
        );

        assert_eq!(rx.recv().await.unwrap().as_ref(), "你🙂".as_bytes());
        assert!(rx.try_recv().is_err());
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn committed_ime_text_bypasses_report_all_key_encoding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 2);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input("你".as_bytes().to_vec());

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static("你".as_bytes())
        );
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn committed_ascii_uppercase_bypasses_report_all_key_encoding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 2);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"A".to_vec());

        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from_static(b"A"));
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn committed_text_does_not_erase_owned_physical_key() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 4);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"\x1b[106u".to_vec());
        app.route_client_input(b"j".to_vec());
        app.route_client_input(b"\x1b[106;1:3u".to_vec());

        assert_eq!(
            rx.try_recv().expect("physical press"),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            rx.try_recv().expect("committed text"),
            bytes::Bytes::from_static(b"j")
        );
        assert_eq!(
            rx.try_recv().expect("physical release"),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn outer_focus_loss_releases_owned_report_all_keys() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 3);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_events(
            vec![
                raw_key(
                    KeyCode::Char('j'),
                    KeyModifiers::empty(),
                    KeyEventKind::Press,
                ),
                crate::raw_input::RawInputEvent::OuterFocusLost,
            ],
            false,
        );

        assert_eq!(
            rx.try_recv().expect("forwarded press"),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            rx.try_recv().expect("synthetic release on focus loss"),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn a_focused_notepad_takes_client_keys_before_the_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.notepad.enabled = true;
        app.state.notepad.set_visible_tabs(Vec::new());
        app.state.notepad.focused = true;

        app.route_client_events_from(
            42,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );

        assert_eq!(app.state.notepad.body(), "j\n");
        assert!(
            rx.try_recv().is_err(),
            "the pane must not see keys typed into the notepad"
        );
    }

    #[tokio::test]
    async fn toggling_hidden_notes_does_not_steal_input_when_notes_are_shown() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("hidden-notes-toggle");
        let focused = workspace.focused_pane_id().expect("focused pane");
        let (runtime, mut pane_input) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_sections_layout = true;
        app.state.sidebar_areas.notes = false;
        app.state.notepad.enabled = true;

        assert!(!app.state.notepad.focused);
        assert!(!app.state.toggle_notepad_focus());
        assert!(!app.state.notepad.focused);
        app.state.set_notepad_focus(true);
        assert!(!app.state.notepad.focused);

        app.state.sidebar_areas.notes = true;
        assert_eq!(app.state.input_owner(), state::InputOwner::Pane);
        app.route_client_events_from(
            42,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );

        assert!(pane_input.try_recv().is_ok(), "the pane receives the key");
        assert!(app.state.notepad.body().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn config_reload_hiding_focused_notes_routes_keys_back_to_the_pane() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("notes-focus-reload");
        std::fs::create_dir_all(path.parent().expect("config parent"))
            .expect("create config directory");
        std::fs::write(
            &path,
            "[ui.sidebar]\nlayout = \"sections\"\n[ui.sidebar.areas]\nnotes = true\n[notepad]\nenabled = true\n",
        )
        .expect("write visible-notes config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().expect("focused pane");
        let (runtime, mut pane_input) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_sections_layout = true;
        app.state.sidebar_areas.notes = true;
        app.state.notepad.enabled = true;
        app.state.notepad.focused = true;
        assert_eq!(app.state.input_owner(), state::InputOwner::Notepad);

        std::fs::write(
            &path,
            "[ui.sidebar]\nlayout = \"sections\"\n[ui.sidebar.areas]\nnotes = false\n[notepad]\nenabled = true\n",
        )
        .expect("write hidden-notes config");
        app.reload_config();

        assert!(
            !app.state.notepad.focused,
            "reload releases hidden-notes focus"
        );
        assert_eq!(app.state.input_owner(), state::InputOwner::Pane);
        app.route_client_events_from(
            42,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );
        assert!(pane_input.try_recv().is_ok(), "the pane receives the key");
        assert!(app.state.notepad.body().is_empty());

        std::fs::write(
            &path,
            "[ui.sidebar]\nlayout = \"sections\"\n[ui.sidebar.areas]\nnotes = true\n[notepad]\nenabled = true\n",
        )
        .expect("write visible-notes config again");
        app.reload_config();

        assert!(!app.state.notepad.focused);
        assert_eq!(app.state.input_owner(), state::InputOwner::Pane);
        app.route_client_events_from(
            42,
            vec![raw_key(
                KeyCode::Char('k'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );
        assert!(
            pane_input.try_recv().is_ok(),
            "the pane keeps input ownership"
        );
        assert!(app.state.notepad.body().is_empty());

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(path.parent().expect("config parent")).ok();
    }

    #[tokio::test]
    async fn headless_subgroup_picker_takes_keys_before_focused_notepad() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut pane_input) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.notepad.enabled = true;
        app.state.notepad.focused = true;
        let note_before = app.state.notepad.body().to_string();
        app.state.sidebar_subgroup_picker = Some(state::SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 0,
            anchor: (7, 4),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });

        app.route_client_events_from(
            42,
            vec![raw_key(
                KeyCode::Char('a'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );

        assert_eq!(
            app.state
                .sidebar_subgroup_picker
                .as_ref()
                .map(|picker| picker.filter.query.as_str()),
            Some("a")
        );
        assert_eq!(app.state.notepad.body(), note_before);
        assert!(pane_input.try_recv().is_err());
    }

    #[tokio::test]
    async fn headless_subgroup_picker_takes_keys_before_full_frame_surfaces() {
        for overlay in [
            "Symphony",
            "Loop History",
            "Usage",
            "Work",
            "dock preview",
            "Inbox",
        ] {
            let mut app = test_app();
            let mut workspace = Workspace::test_new("test");
            let focused = workspace.focused_pane_id().unwrap();
            let (runtime, mut pane_input) = TerminalRuntime::test_with_channel(80, 24);
            workspace.tabs[0].runtimes.insert(focused, runtime);
            app.state.workspaces = vec![workspace];
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.set_server_mode(Mode::Terminal);
            app.state.sidebar_subgroup_picker = Some(state::SidebarSubgroupPickerState {
                ws_idx: 0,
                tab_idx: 0,
                anchor: (7, 4),
                filter: crate::ui::dropdown::DropdownFilterState::default(),
            });
            match overlay {
                "Symphony" => app.state.toggle_symphony(),
                "Loop History" => app.state.toggle_loop_run_history(),
                "Usage" => app.state.toggle_usage_view(),
                "Work" => {
                    app.state.work_view = Some(state::WorkViewState::new(false, None));
                }
                "dock preview" => {
                    app.state.dock_collapsed = true;
                    app.state.dock_object_preview = Some(state::DockObjectRef {
                        surface: state::DockSurface::Linear,
                        key: "SCA-1".into(),
                    });
                }
                "Inbox" => app.state.toggle_inbox(),
                _ => unreachable!(),
            }

            app.route_client_events_from(
                42,
                vec![raw_key(
                    KeyCode::Char('7'),
                    KeyModifiers::empty(),
                    KeyEventKind::Press,
                )],
                false,
            );

            assert_eq!(
                app.state
                    .sidebar_subgroup_picker
                    .as_ref()
                    .map(|picker| picker.filter.query.as_str()),
                Some("7"),
                "the client subgroup picker must retain input precedence over {overlay}"
            );
            assert!(
                pane_input.try_recv().is_err(),
                "the subgroup picker must not leak keys into the focused pane behind {overlay}"
            );
        }
    }

    #[tokio::test]
    async fn disconnected_input_source_releases_owned_report_all_keys() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 3);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_events_from(
            42,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );
        app.clear_input_source(42);

        assert_eq!(
            rx.try_recv().expect("forwarded press"),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            rx.try_recv().expect("synthetic release on disconnect"),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn physical_count_one_repeats_and_release_keep_the_pressed_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let pressed_pane = workspace.focused_pane_id().unwrap();
        let other_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(pressed_pane);
        let (pressed_runtime, mut pressed_rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 5);
        let (other_runtime, mut other_rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 2);
        workspace.tabs[0]
            .runtimes
            .insert(pressed_pane, pressed_runtime);
        workspace.tabs[0].runtimes.insert(other_pane, other_runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let record = crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 65,
            virtual_scan_code: 30,
            unicode: 97,
            control_key_state: 0,
        };
        let roundtrip = |events: Vec<crate::protocol::ClientInputEvent>| {
            let message = crate::protocol::ClientMessage::InputEvents { events };
            let encoded =
                bincode::serde::encode_to_vec(&message, bincode::config::standard()).unwrap();
            let (decoded, _): (crate::protocol::ClientMessage, _) =
                bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
            let crate::protocol::ClientMessage::InputEvents { events } = decoded else {
                panic!("expected structured input events");
            };
            events
                .into_iter()
                .map(|event| event.to_raw_input_event())
                .collect()
        };
        let event = |kind, record| crate::protocol::ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('a'),
            modifiers: 0,
            kind,
            repeat_count: 1,
            generated_text: None,
            source: crate::protocol::ClientKeySource::WindowsConsole { record },
        };

        app.route_client_events(
            roundtrip(vec![event(crate::protocol::ClientKeyKind::Press, record)]),
            false,
        );
        assert!(app.state.focus_pane_in_workspace(0, other_pane));
        app.route_client_events(
            roundtrip(vec![
                event(crate::protocol::ClientKeyKind::Press, record),
                event(crate::protocol::ClientKeyKind::Repeat, record),
                event(
                    crate::protocol::ClientKeyKind::Release,
                    crate::input::WindowsKeyRecord {
                        key_down: false,
                        unicode: 0,
                        ..record
                    },
                ),
            ]),
            false,
        );

        for expected in [
            b"\x1b[97;1:1u".as_slice(),
            b"\x1b[97;1:2u".as_slice(),
            b"\x1b[97;1:2u".as_slice(),
            b"\x1b[97;1:3u".as_slice(),
        ] {
            assert_eq!(pressed_rx.try_recv().unwrap().as_ref(), expected);
        }
        assert!(pressed_rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn grouped_physical_press_and_runtime_loss_preserve_count_then_close_the_lease() {
        let mut app = test_app();
        let workspace = Workspace::test_new("test");
        let pane_id = workspace.focused_pane_id().unwrap();
        let terminal_id = workspace.tabs[0].terminal_id(pane_id).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 2);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let record = crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 3,
            virtual_key_code: 65,
            virtual_scan_code: 30,
            unicode: 97,
            control_key_state: 0,
        };
        let message = crate::protocol::ClientMessage::InputEvents {
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('a'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
                repeat_count: 3,
                generated_text: None,
                source: crate::protocol::ClientKeySource::WindowsConsole { record },
            }],
        };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard()).unwrap();
        let (decoded, _): (crate::protocol::ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        let crate::protocol::ClientMessage::InputEvents { events } = decoded else {
            panic!("expected structured input events");
        };

        app.route_client_events(
            events
                .into_iter()
                .map(|event| event.to_raw_input_event())
                .collect(),
            false,
        );
        app.shutdown_terminal_runtime(terminal_id.clone());

        assert_eq!(
            rx.try_recv().expect("grouped press"),
            bytes::Bytes::from_static(b"\x1b[97;1:1u\x1b[97;1:2u\x1b[97;1:2u")
        );
        assert_eq!(
            rx.try_recv()
                .expect("synthetic release before runtime shutdown"),
            bytes::Bytes::from_static(b"\x1b[97;1:3u")
        );
        assert!(rx.try_recv().is_err());
        assert!(app.input_leases.is_empty());
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
    }

    #[tokio::test]
    async fn report_all_repeat_and_release_return_to_the_pressed_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let pressed_pane = workspace.focused_pane_id().unwrap();
        let other_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(pressed_pane);
        let (pressed_runtime, mut pressed_rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 4);
        let (other_runtime, mut other_rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 4);
        workspace.tabs[0]
            .runtimes
            .insert(pressed_pane, pressed_runtime);
        workspace.tabs[0].runtimes.insert(other_pane, other_runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"\x1b[106u".to_vec());
        assert!(app.state.focus_pane_in_workspace(0, other_pane));
        app.route_client_input(b"\x1b[106;1:2u\x1b[106;1:3u".to_vec());

        assert_eq!(
            pressed_rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            pressed_rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:2u")
        );
        assert_eq!(
            pressed_rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(pressed_rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn report_all_key_ownership_is_isolated_by_client() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let first_pane = workspace.focused_pane_id().unwrap();
        let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(first_pane);
        let (first_runtime, mut first_rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 3);
        let (second_runtime, mut second_rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 3);
        workspace.tabs[0].runtimes.insert(first_pane, first_runtime);
        workspace.tabs[0]
            .runtimes
            .insert(second_pane, second_runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_events_from(
            1,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );
        assert!(app.state.focus_pane_in_workspace(0, second_pane));
        app.route_client_events_from(
            2,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Press,
            )],
            false,
        );
        app.route_client_events_from(
            1,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Release,
            )],
            false,
        );
        app.route_client_events_from(
            2,
            vec![raw_key(
                KeyCode::Char('j'),
                KeyModifiers::empty(),
                KeyEventKind::Release,
            )],
            false,
        );

        for rx in [&mut first_rx, &mut second_rx] {
            assert_eq!(
                rx.recv().await.unwrap(),
                bytes::Bytes::from_static(b"\x1b[106;1:1u")
            );
            assert_eq!(
                rx.recv().await.unwrap(),
                bytes::Bytes::from_static(b"\x1b[106;1:3u")
            );
            assert!(rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn report_all_release_survives_modifier_release_order() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 3);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"\x1b[106:74;2u\x1b[106;1:3u".to_vec());

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106:74;2:1u")
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn route_client_input_does_not_forward_release_for_consumed_prefix() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>15u", 2);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"\x1b[98;5u\x1b[98;5:3u".to_vec());

        assert_eq!(app.state.server_mode(), Mode::Prefix);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn route_client_input_preserves_shift_enter_for_modify_other_keys_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        runtime.test_process_pty_bytes(b"\x1b[>4;1m");
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"\x1b[13;2u".to_vec());

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[27;2;13~")
        );
    }

    #[tokio::test]
    async fn route_client_input_splits_multi_event_payloads_before_forwarding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"ab".to_vec());

        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from_static(b"a"));
        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from_static(b"b"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn route_client_input_forwards_multilingual_ime_text_to_focused_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let text = "中日한🙂";
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_capacity(80, 24, text.chars().count());
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(text.as_bytes().to_vec());

        let mut forwarded = Vec::new();
        for _ in text.chars() {
            let chunk = rx.recv().await.unwrap();
            forwarded.extend_from_slice(&chunk);
        }
        assert_eq!(forwarded, text.as_bytes());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn route_client_input_forwards_long_voice_like_cjk_text_without_truncation() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let text = "你好，今天我们测试一段比较长的语音输入。こんにちは。안녕하세요.🙂".repeat(64);
        let char_count = text.chars().count();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel_capacity(80, 24, char_count);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(text.as_bytes().to_vec());

        let mut forwarded = Vec::new();
        for _ in 0..char_count {
            let chunk = rx.recv().await.unwrap();
            forwarded.extend_from_slice(&chunk);
        }
        assert_eq!(forwarded, text.as_bytes());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn route_client_input_handles_mouse_events() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;

        // Send a mouse scroll-up event via SGR encoding.
        let mouse_bytes = b"\x1b[<64;10;5M".to_vec();
        // This should not panic even though mouse handling is simplified
        // in headless mode.
        app.route_client_input(mouse_bytes);
        // No assertions on specific behavior — just no panic.
    }

    #[test]
    fn route_client_input_advances_onboarding_modal() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::Onboarding);

        app.route_client_input(b"\r".to_vec());

        assert_eq!(app.state.server_mode(), Mode::Settings);
        assert_eq!(
            app.state.settings.section,
            state::SettingsSection::Integrations
        );
    }

    #[test]
    fn route_client_input_pastes_bracketed_text_into_rename_modal() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        app.state.name_input = "2".into();
        app.state.name_input_replace_on_type = true;

        app.route_client_input(b"\x1b[200~feature/logs\x1b[201~".to_vec());

        assert_eq!(app.state.name_input, "feature/logs");
        assert!(!app.state.name_input_replace_on_type);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn route_client_pastes_into_home_composer_without_forwarding_to_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.home = Some(home::HomeState::default());

        app.route_client_events(
            vec![
                crate::raw_input::RawInputEvent::Paste("first".into()),
                crate::raw_input::RawInputEvent::Paste(" second".into()),
                crate::raw_input::RawInputEvent::Paste(" third".into()),
            ],
            true,
        );

        assert_eq!(
            app.state.home.as_ref().map(|home| home.prompt.as_str()),
            Some("first second third")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn route_client_paste_resumes_settled_pane_before_forwarding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("settled-client-paste");
        let pane_id = workspace.tabs[0].root_pane;
        let (runtime, mut input_rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_000));
        assert!(app.flush_pane_settlement_events());

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste("reply".into())],
            false,
        );

        assert_eq!(
            input_rx.try_recv().expect("forwarded paste").as_ref(),
            b"reply"
        );
        assert!(!app.state.pane_is_settled(0, pane_id));
        assert!(app.state.pending_pane_settlement_changes.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    // AC1: home paste routing stays out of a remotely leased pane and leaves that lease intact.
    async fn home_paste_does_not_write_through_remote_lease() {
        let (mut app, terminal_id, mut input_rx) = test_app_with_focused_runtime();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        assert!(app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("focused runtime")
            .acquire_remote_owner(41));
        let controlled_owners = std::collections::HashMap::from([(terminal_id, 41)]);
        app.state.home = Some(home::HomeState::default());

        app.route_client_events_from_with_human_input_hook(
            9,
            vec![crate::raw_input::RawInputEvent::Paste("home prompt".into())],
            true,
            &mut |_| {},
            Some(&controlled_owners),
        );

        assert_eq!(
            app.state.home.as_ref().map(|home| home.prompt.as_str()),
            Some("home prompt")
        );
        assert!(input_rx.try_recv().is_err());
        assert!(!app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("focused runtime")
            .acquire_remote_owner(99));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn route_client_paste_is_swallowed_by_dock_object_preview() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_object_preview = Some(state::DockObjectRef {
            surface: state::DockSurface::Linear,
            key: "SCA-1".into(),
        });

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste("hidden".into())],
            true,
        );

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn route_client_paste_retires_blocked_hook_authority_after_forwarding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            Some(1),
        );

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste("continue".into())],
            true,
        );

        assert!(rx.try_recv().is_ok());
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            AgentState::Idle
        );
        assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
    }

    #[test]
    fn route_client_input_rename_enter_submits_through_api_path() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("old")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
        app.state.rename_target = Some(state::RenameTarget::Workspace {
            workspace_id: app.state.workspaces[0].id.clone(),
        });
        app.state.name_input = "new".into();

        app.route_client_input(b"\r".to_vec());

        assert_eq!(app.state.workspaces[0].custom_name.as_deref(), Some("new"));
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceRenamed)
        }));
    }

    #[test]
    fn route_client_input_context_menu_enter_submits_through_api_path() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.confirm_close = false;
        app.state.context_menu = Some(state::ContextMenuState {
            kind: state::ContextMenuKind::Workspace {
                workspace_id: app.state.workspaces[1].id.clone(),
                ws_idx: 1,
            },
            x: 2,
            y: 2,
            selected: state::ContextMenuAction::CloseWorkspace,
        });
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::ContextMenu);

        app.route_client_input(b"\r".to_vec());

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[test]
    fn raw_ctrl_v_decodes_as_modal_paste_shortcut() {
        let events = crate::raw_input::parse_raw_input_bytes_sync(&[0x16]);
        let Some(crate::raw_input::RawInputEvent::Key(key)) = events.first() else {
            panic!("expected ctrl-v key event");
        };

        assert!(input::is_modal_paste_shortcut(&key.as_key_event()));
    }

    #[test]
    fn route_client_events_pastes_text_into_new_linked_worktree_modal() {
        let mut app = test_app();
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::NewLinkedWorktree);
        app.state.name_input = "generated-branch".into();
        app.state.name_input_replace_on_type = true;
        app.state.worktree_create = Some(state::WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "generated-branch".into(),
            checkout_path: "/repo/herdr-generated-branch".into(),
            error: None,
            creating: false,
        });

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste(
                "feature/linear-302".into(),
            )],
            true,
        );

        assert_eq!(app.state.name_input, "feature/linear-302");
        assert_eq!(
            app.state
                .worktree_create
                .as_ref()
                .map(|create| create.branch.as_str()),
            Some("feature/linear-302")
        );
    }

    #[tokio::test]
    async fn route_client_events_pastes_only_into_visible_popup() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("tiled");
        let focused = workspace.focused_pane_id().unwrap();
        let (tiled_runtime, mut tiled_rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, tiled_runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let (popup_runtime, mut popup_rx) = TerminalRuntime::test_with_channel(40, 12);
        app.install_test_popup_runtime(popup_runtime);
        assert!(app
            .state
            .should_capture_host_mouse_from(&app.terminal_runtimes));

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste("popup-only".into())],
            true,
        );

        assert_eq!(
            popup_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"popup-only")
        );
        assert!(tiled_rx.try_recv().is_err());

        app.route_client_events(
            vec![raw_key(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )],
            true,
        );
        assert_eq!(
            popup_rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"x")
        );
        assert!(tiled_rx.try_recv().is_err());

        app.state.set_server_mode(Mode::Settings);
        assert!(
            !app.handle_raw_input_event(raw_key(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            ))
            .await,
            "a popup behind Settings must not receive a repeated key"
        );
        assert!(popup_rx.try_recv().is_err());
        assert!(tiled_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn route_client_events_discards_paste_when_popup_runtime_is_missing() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("tiled");
        let focused = workspace.focused_pane_id().unwrap();
        let (tiled_runtime, mut tiled_rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, tiled_runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let install_missing_popup = |app: &mut App| {
            let popup_terminal_id = crate::terminal::TerminalId::alloc();
            app.state.terminals.insert(
                popup_terminal_id.clone(),
                crate::terminal::TerminalState::new(
                    popup_terminal_id.clone(),
                    std::path::PathBuf::from("/popup"),
                ),
            );
            app.state.popup_pane = Some(state::PopupPaneState {
                pane_id: crate::layout::PaneId::alloc(),
                terminal_id: popup_terminal_id,
                width: None,
                height: None,
            });
        };
        install_missing_popup(&mut app);

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste("discard-me".into())],
            true,
        );

        assert!(tiled_rx.try_recv().is_err());
        assert!(app.state.popup_pane.is_none());

        install_missing_popup(&mut app);
        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Paste(
                "discard-monolithic".into(),
            ))
            .await
        );
        assert!(tiled_rx.try_recv().is_err());
        assert!(app.state.popup_pane.is_none());
    }

    #[tokio::test]
    async fn popup_mouse_motion_preserves_scrollback() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::Terminal);
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        let (popup_runtime, mut popup_rx) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            40,
            2,
            1024,
            b"one\r\ntwo\r\nthree\r\n\x1b[?1003h\x1b[?1006h",
            4,
        );
        popup_runtime.scroll_up(1);
        assert!(popup_runtime
            .scroll_metrics()
            .is_some_and(|metrics| metrics.offset_from_bottom > 0));
        app.install_test_popup_runtime(popup_runtime);
        let (_, inner) =
            crate::ui::popup_pane_rects(&app.state, app.state.view.terminal_area).unwrap();

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Moved,
                    column: inner.x + 1,
                    row: inner.y,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            )],
            true,
        );

        assert!(popup_rx.try_recv().is_ok());
        assert!(app
            .popup_runtime()
            .and_then(TerminalRuntime::scroll_metrics)
            .is_some_and(|metrics| metrics.offset_from_bottom > 0));
    }

    #[tokio::test]
    async fn route_client_events_routes_popup_mouse_when_global_capture_is_disabled() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("tiled");
        let focused = workspace.focused_pane_id().unwrap();
        let (tiled_runtime, mut tiled_rx) = TerminalRuntime::test_with_channel(80, 24);
        tiled_runtime.test_process_pty_bytes(b"\x1b[?1000h\x1b[?1006h");
        workspace.tabs[0].runtimes.insert(focused, tiled_runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.mouse_capture = false;
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);

        let (popup_runtime, mut popup_rx) = TerminalRuntime::test_with_channel(40, 12);
        popup_runtime.test_process_pty_bytes(b"\x1b[?1000h\x1b[?1006h");
        app.install_test_popup_runtime(popup_runtime);
        let (_, inner) =
            crate::ui::popup_pane_rects(&app.state, app.state.view.terminal_area).unwrap();

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: inner.x,
                    row: inner.y,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            )],
            true,
        );

        assert!(popup_rx.try_recv().is_ok());
        assert!(tiled_rx.try_recv().is_err());

        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: inner.x + 1,
                    row: inner.y,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            ))
            .await
        );
        assert!(popup_rx.try_recv().is_ok());
        assert!(tiled_rx.try_recv().is_err());
    }

    #[test]
    fn route_client_input_closes_release_notes_modal() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::ReleaseNotes);
        app.state.release_notes = Some(release_notes_state());

        app.route_client_input(b"\x1b".to_vec());

        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert!(app.state.release_notes.is_none());
    }

    #[test]
    fn route_client_input_closes_settings_modal() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Settings);
        app.state.settings.original_theme = Some(app.state.theme_name.clone());
        app.state.settings.original_palette = Some(app.state.palette.clone());

        app.route_client_input(b"\x1b".to_vec());

        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn route_client_input_updates_host_terminal_theme_from_osc_response() {
        let mut app = test_app();

        app.route_client_input(b"\x1b]11;#123456\x07\x1b]4;7;rgb:aaaa/bbbb/cccc\x1b\\".to_vec());

        assert_eq!(
            app.state.host_terminal_theme.background,
            Some(crate::terminal_theme::RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            })
        );
        assert_eq!(
            app.state.host_terminal_theme.palette[7],
            Some(crate::terminal_theme::RgbColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            })
        );

        app.route_client_input(crate::raw_input::GHOSTTY_COLOR_SCHEME_DARK_REPORT.to_vec());
        assert_eq!(
            app.state.host_terminal_theme.palette[7],
            Some(crate::terminal_theme::RgbColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            })
        );

        app.route_client_input(b"\x1b]4;7;rgb:dddd/eeee/ffff\x1b\\".to_vec());
        assert_eq!(
            app.state.host_terminal_theme.palette[7],
            Some(crate::terminal_theme::RgbColor {
                r: 0xdd,
                g: 0xee,
                b: 0xff,
            })
        );
    }

    #[tokio::test]
    async fn route_client_input_does_not_forward_incomplete_osc_introducer_to_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel_capacity(80, 24, 1);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.route_client_input(b"\x1b]".to_vec());

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn parse_raw_input_bytes_with_ranges_tracks_offsets() {
        // Verify that the range-aware parser correctly tracks byte offsets
        // for events within a multi-event input buffer.
        let input = b"\x1b[Aa".to_vec(); // Up arrow + 'a'
        let events = crate::raw_input::parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(events.len(), 2, "should parse Up arrow and 'a'");
        // Up arrow: \x1b[A = 3 bytes starting at offset 0
        assert_eq!(events[0].start, 0);
        assert_eq!(events[0].len, 3);
        // 'a': 1 byte starting at offset 3
        assert_eq!(events[1].start, 3);
        assert_eq!(events[1].len, 1);

        // Verify the raw bytes for each event are correct.
        assert_eq!(
            &input[events[0].start..events[0].start + events[0].len],
            b"\x1b[A"
        );
        assert_eq!(
            &input[events[1].start..events[1].start + events[1].len],
            b"a"
        );
    }
}
