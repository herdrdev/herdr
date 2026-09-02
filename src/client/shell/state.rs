use super::*;

pub(super) const MIN_TAB_WIDTH: u16 = 8;
pub(super) const NEW_TAB_WIDTH: u16 = 3;
pub(super) const WORKSPACE_HEADER_ROWS: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientShellKeybindingSource {
    Local,
    RemoteLocal,
    Endpoint,
}

pub(crate) struct ClientShellConfig {
    pub(super) sidebar_width: u16,
    pub(super) sidebar_min_width: u16,
    pub(super) sidebar_max_width: u16,
    pub(super) sidebar_start_collapsed: bool,
    pub(super) sidebar_collapsed_mode: SidebarCollapsedModeConfig,
    pub(super) mobile_width_threshold: u16,
    pub(super) tab_bar_position: TabBarPositionConfig,
    pub(super) hide_tab_bar_when_single_tab: bool,
    pub(super) spaces: SpacesSidebarConfig,
    pub(super) agents: crate::config::AgentsSidebarConfig,
    pub(super) agent_panel_sort: crate::config::AgentPanelSortConfig,
    pub(super) status_indicators: crate::config::StatusIndicatorStyle,
    pub(super) sound_enabled: bool,
    pub(super) toast_delivery: crate::config::ToastDelivery,
    pub(super) toast_delay_seconds: u64,
    pub(super) toast_position: crate::config::ToastHerdrPosition,
    pub(super) copy_on_select: bool,
    pub(super) clipboard_toast_enabled: bool,
    pub(super) clipboard_toast_position: crate::config::ToastClipboardPosition,
    pub(super) theme_name: String,
    pub(super) theme_runtime: crate::app::state::ThemeRuntimeConfig,
    pub(super) palette: Palette,
    pub(super) keybinds: LiveKeybindConfig,
    pub(super) local_keys: crate::config::KeysConfig,
    pub(super) keybinding_source: ClientShellKeybindingSource,
    pub(super) prompt_new_tab_name: bool,
    pub(super) prompt_new_workspace_name: bool,
    pub(super) confirm_close: bool,
    pub(super) mouse_capture: bool,
    pub(super) mouse_scroll_lines: usize,
    pub(super) right_click_passthrough_modifiers: Option<crossterm::event::KeyModifiers>,
    pub(super) redraw_on_focus_gained: bool,
    pub(super) switch_ascii_input_source_in_prefix: bool,
    pub(super) local_config_path: std::path::PathBuf,
    pub(super) preferences_path: Option<std::path::PathBuf>,
    pub(super) preferences: preferences::ClientChromePreferences,
    pub(super) startup_config_diagnostic: Option<String>,
    pub(super) startup_onboarding: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClientShellLayout {
    pub sidebar: Rect,
    pub tab_bar: Rect,
    pub mobile_header: Rect,
    pub pane_surface: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClientMobileTarget {
    NewWorkspace,
    Workspace(String),
    NewTab,
    Tab(String),
    Agent(String),
    Menu(usize),
}

#[derive(Default)]
pub(super) struct ShellHitMap {
    pub(super) workspaces: Vec<WorkspaceHit>,
    pub(super) workspace_body: Rect,
    pub(super) workspace_scrollbar: Rect,
    pub(super) workspace_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) workspace_max_scroll: usize,
    pub(super) tabs: Vec<(Rect, String)>,
    pub(super) panes: Vec<PaneHit>,
    pub(super) popup: Option<PaneHit>,
    pub(super) pane_splits: Vec<PaneSplitHit>,
    pub(super) agents: Vec<(Rect, String)>,
    pub(super) agent_body: Rect,
    pub(super) agent_scrollbar: Rect,
    pub(super) agent_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) agent_max_scroll: usize,
    pub(super) agent_sort_toggle: Rect,
    pub(super) sidebar_divider: Rect,
    pub(super) sidebar_section_divider: Rect,
    pub(super) sidebar_toggle: Rect,
    pub(super) new_workspace: Rect,
    pub(super) new_tab: Rect,
    pub(super) tab_scroll_left: Rect,
    pub(super) tab_scroll_right: Rect,
    pub(super) mobile_switch: Rect,
    pub(super) mobile_close: Rect,
    pub(super) mobile_targets: Vec<(Rect, ClientMobileTarget)>,
    pub(super) mobile_max_scroll: usize,
    pub(super) global_launcher: Rect,
    pub(super) notification_toast: Rect,
    pub(super) global_menu_rows: Vec<(Rect, usize)>,
    pub(super) context_menu_rows: Vec<(Rect, usize)>,
    pub(super) overlay_primary: Rect,
    pub(super) overlay_clear: Rect,
    pub(super) overlay_cancel: Rect,
    pub(super) navigator_popup: Rect,
    pub(super) navigator_search: Rect,
    pub(super) navigator_rows: Vec<(Rect, usize)>,
    pub(super) worktree_search: Rect,
    pub(super) worktree_rows: Vec<(Rect, usize)>,
    pub(super) help_popup: Rect,
    pub(super) help_scrollbar: Rect,
    pub(super) help_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) help_max_scroll: usize,
    pub(super) settings_popup: Rect,
    pub(super) settings_tabs: Vec<(Rect, ClientSettingsSection)>,
    pub(super) settings_choices: Vec<(Rect, usize)>,
    pub(super) product_announcement_scrollbar: Rect,
    pub(super) product_announcement_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) product_announcement_max_scroll: usize,
    pub(super) release_notes_scrollbar: Rect,
    pub(super) release_notes_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) release_notes_max_scroll: usize,
}

#[derive(Clone)]
pub(super) struct PaneHit {
    pub(super) rect: Rect,
    pub(super) inner_rect: Rect,
    pub(super) scrollbar_rect: Option<Rect>,
    pub(super) scroll: Option<crate::pane::ScrollMetrics>,
    pub(super) pane_id: String,
    pub(super) popup: bool,
    pub(super) mouse_reporting: bool,
    pub(super) sgr_pixel_mouse: bool,
    pub(super) pixel_width: u32,
    pub(super) pixel_height: u32,
}

#[derive(Clone)]
pub(super) struct PaneSplitHit {
    pub(super) direction: crate::protocol::PaneSurfaceSplitDirection,
    pub(super) pos: u16,
    pub(super) area: Rect,
    pub(super) hit_rect: Rect,
    pub(super) path: Vec<bool>,
    pub(super) topology_signature: u64,
}

pub(super) struct ClientPaneMouseGesture {
    pub(super) hit: PaneHit,
    pub(super) button: crossterm::event::MouseButton,
    pub(super) stripped_modifiers: crossterm::event::KeyModifiers,
    pub(super) last_event: crossterm::event::MouseEvent,
    pub(super) last_position: crate::protocol::ClientMousePosition,
}

pub(super) struct ClientWorkspacePress {
    pub(super) workspace_id: String,
    pub(super) start_column: u16,
    pub(super) start_row: u16,
}

pub(super) struct ClientTabPress {
    pub(super) tab_id: String,
    pub(super) workspace_id: String,
    pub(super) start_column: u16,
    pub(super) start_row: u16,
}

pub(super) enum ClientChromeDrag {
    SidebarWidth,
    SidebarSection,
    WorkspaceScrollbar {
        grab_row_offset: u16,
    },
    AgentScrollbar {
        grab_row_offset: u16,
    },
    HelpScrollbar {
        grab_row_offset: u16,
    },
    ProductAnnouncementScrollbar {
        grab_row_offset: u16,
    },
    ReleaseNotesScrollbar {
        grab_row_offset: u16,
    },
    Tab {
        tab_id: String,
        workspace_id: String,
        insert_index: Option<usize>,
    },
    Workspace {
        source_workspace_id: String,
        target: Option<(Option<String>, u16)>,
    },
    PaneSplit {
        hit: PaneSplitHit,
        tab_id: String,
        grab_offset: i32,
        last_sent_ratio: Option<f32>,
        last_sent_at: Option<std::time::Instant>,
    },
    PaneScrollbar {
        hit: PaneHit,
        grab_row_offset: u16,
        last_sent_offset: Option<usize>,
        last_sent_at: Option<std::time::Instant>,
    },
}

pub(super) struct WorkspaceHit {
    pub(super) rect: Rect,
    pub(super) workspace_id: String,
    pub(super) indented: bool,
    pub(super) group_toggle: Option<(Rect, String)>,
}

#[derive(Debug)]
pub(crate) enum ClientShellAction {
    Endpoint {
        boot_id: String,
        request: Box<crate::api::schema::Request>,
    },
    ClipboardWrite(Vec<u8>),
    Request(ClientMessage),
    OpenSafeWebUrl(String),
    ReplayMouse(Vec<crossterm::event::MouseEvent>),
    Keybind(crate::input::KeybindAction),
}

#[derive(Default)]
pub(crate) struct ClientShellInput {
    pub detach: bool,
    pub repaint: bool,
    pub resize: bool,
    pub query_host_appearance: bool,
    pub query_host_theme: bool,
    pub requests: Vec<ClientMessage>,
    pub actions: Vec<ClientShellAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientShellMode {
    Terminal,
    Prefix,
    Navigate,
    Resize,
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientShellOverlayKind {
    Onboarding,
    ProductAnnouncement,
    ReleaseNotes,
    Rename,
    ConfirmClose,
    Help,
    Navigator,
    WorktreeCreate,
    WorktreeOpen,
    WorktreeRemove,
    ContextMenu,
    GlobalMenu,
    Settings,
}

#[derive(Debug)]
pub(super) enum ClientRenameTarget {
    NewWorkspace {
        source_workspace_id: Option<String>,
        cwd: Option<String>,
        suggested_name: String,
    },
    Workspace {
        workspace_id: String,
    },
    NewTab {
        workspace_id: String,
        default_name: String,
    },
    Tab {
        tab_id: String,
        auto_name: bool,
        original_name: String,
    },
    Pane {
        pane_id: String,
    },
}

#[derive(Debug)]
pub(super) struct ClientRenameOverlay {
    pub(super) title: &'static str,
    pub(super) input: String,
    pub(super) replace_on_type: bool,
    pub(super) target: ClientRenameTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientNavigatorFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

#[derive(Clone, Debug)]
pub(super) enum ClientNavigatorTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
}

#[derive(Clone, Debug)]
pub(super) struct ClientNavigatorRow {
    pub(super) depth: u8,
    pub(super) label: String,
    pub(super) meta: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) current: bool,
    pub(super) target: ClientNavigatorTarget,
}

#[derive(Debug)]
pub(super) struct ClientNavigatorOverlay {
    pub(super) query: String,
    pub(super) search_focused: bool,
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) filter: Option<ClientNavigatorFilter>,
    pub(super) expanded_workspaces: HashSet<String>,
}

#[derive(Debug)]
pub(super) struct ClientHelpOverlay {
    pub(super) query: String,
    pub(super) search_focused: bool,
    pub(super) scroll: usize,
}

#[derive(Debug)]
pub(super) struct ClientGlobalMenuOverlay {
    pub(super) highlighted: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientSettingsSection {
    Theme,
    Indicators,
    Sound,
    Toast,
    Integrations,
}

impl ClientSettingsSection {
    pub(super) const ALL: &[Self] = &[
        Self::Theme,
        Self::Indicators,
        Self::Sound,
        Self::Toast,
        Self::Integrations,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Indicators => "indicators",
            Self::Sound => "sound",
            Self::Toast => "toasts",
            Self::Integrations => "integrations",
        }
    }
}

#[derive(Debug)]
pub(super) struct ClientSettingsOverlay {
    pub(super) section: ClientSettingsSection,
    pub(super) selected: usize,
    pub(super) original_theme_name: String,
    pub(super) original_palette: Palette,
    pub(super) integrations: Vec<crate::api::schema::IntegrationInfo>,
    pub(super) integration_messages: Vec<String>,
    pub(super) loading_integrations: bool,
    pub(super) installing_integrations: bool,
}

#[derive(Debug)]
pub(super) struct ClientWorktreeCreateOverlay {
    pub(super) source_workspace_id: String,
    pub(super) repo_name: String,
    pub(super) branch: String,
    pub(super) checkout_path: String,
    pub(super) replace_on_type: bool,
    pub(super) error: Option<String>,
    pub(super) creating: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ClientWorktreeOpenEntry {
    pub(super) path: String,
    pub(super) branch: Option<String>,
    pub(super) is_linked_worktree: bool,
    pub(super) is_detached: bool,
    pub(super) open_workspace_id: Option<String>,
    pub(super) label: String,
}

impl ClientWorktreeOpenEntry {
    pub(super) fn status_label(&self) -> &'static str {
        if self.open_workspace_id.is_some() {
            "open"
        } else if self.branch.is_some() {
            ""
        } else if self.is_detached && self.is_linked_worktree {
            "detached"
        } else {
            "root"
        }
    }

    pub(super) fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || format!(
                "{} {} {} {}",
                self.label,
                self.branch.as_deref().unwrap_or_default(),
                self.path,
                self.status_label()
            )
            .to_lowercase()
            .contains(&query)
    }
}

#[derive(Debug)]
pub(super) struct ClientWorktreeOpenOverlay {
    pub(super) source_workspace_id: String,
    pub(super) entries: Vec<ClientWorktreeOpenEntry>,
    pub(super) selected: usize,
    pub(super) query: String,
    pub(super) search_focused: bool,
    pub(super) error: Option<String>,
    pub(super) opening: bool,
}

impl ClientWorktreeOpenOverlay {
    pub(super) fn filtered_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.matches_query(&self.query).then_some(index))
            .collect()
    }

    pub(super) fn selected_entry_index(&self) -> Option<usize> {
        let filtered = self.filtered_indices();
        filtered
            .contains(&self.selected)
            .then_some(self.selected)
            .or_else(|| filtered.first().copied())
    }
}

#[derive(Debug)]
pub(super) struct ClientWorktreeRemoveOverlay {
    pub(super) workspace_id: String,
    pub(super) path: String,
    pub(super) error: Option<String>,
    pub(super) removing: bool,
    pub(super) force_confirmation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientContextMenuAction {
    Rename,
    Close,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    ToggleGroup,
    NewTab,
    RenamePane,
    ClearPaneName,
    SwapWithFocusedPane,
    SplitRight,
    SplitDown,
    Zoom,
    ToggleRightClickPassthrough,
    ClosePane,
}

#[derive(Debug)]
pub(super) enum ClientContextMenuTarget {
    Workspace {
        workspace_id: String,
        is_git: bool,
        is_linked_worktree: bool,
        has_worktree_children: bool,
        collapsed: bool,
    },
    Tab {
        tab_id: String,
        workspace_id: String,
    },
    Pane {
        pane_id: String,
        workspace_id: String,
        source_pane_id: Option<String>,
        has_manual_label: bool,
        right_click_passthrough: bool,
    },
}

#[derive(Debug)]
pub(super) struct ClientContextMenuOverlay {
    pub(super) target: ClientContextMenuTarget,
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) highlighted: usize,
}

pub(super) struct ClientContextMenuItem {
    pub(super) label: &'static str,
    pub(super) action: ClientContextMenuAction,
}

#[derive(Debug)]
pub(super) struct ClientConfirmCloseOverlay {
    pub(super) workspace_id: String,
    pub(super) title: String,
    pub(super) detail: String,
}

#[derive(Debug)]
pub(super) enum ClientShellOverlay {
    Onboarding,
    ProductAnnouncement(crate::app::state::ProductAnnouncementState),
    ReleaseNotes(crate::app::state::ReleaseNotesState),
    Rename(ClientRenameOverlay),
    ConfirmClose(ClientConfirmCloseOverlay),
    Help(ClientHelpOverlay),
    Navigator(ClientNavigatorOverlay),
    WorktreeCreate(ClientWorktreeCreateOverlay),
    WorktreeOpen(ClientWorktreeOpenOverlay),
    WorktreeRemove(ClientWorktreeRemoveOverlay),
    ContextMenu(ClientContextMenuOverlay),
    GlobalMenu(ClientGlobalMenuOverlay),
    Settings(ClientSettingsOverlay),
}

impl ClientShellOverlay {
    pub(super) fn kind(&self) -> ClientShellOverlayKind {
        match self {
            Self::Onboarding => ClientShellOverlayKind::Onboarding,
            Self::ProductAnnouncement(_) => ClientShellOverlayKind::ProductAnnouncement,
            Self::ReleaseNotes(_) => ClientShellOverlayKind::ReleaseNotes,
            Self::Rename(_) => ClientShellOverlayKind::Rename,
            Self::ConfirmClose(_) => ClientShellOverlayKind::ConfirmClose,
            Self::Help(_) => ClientShellOverlayKind::Help,
            Self::Navigator(_) => ClientShellOverlayKind::Navigator,
            Self::WorktreeCreate(_) => ClientShellOverlayKind::WorktreeCreate,
            Self::WorktreeOpen(_) => ClientShellOverlayKind::WorktreeOpen,
            Self::WorktreeRemove(_) => ClientShellOverlayKind::WorktreeRemove,
            Self::ContextMenu(_) => ClientShellOverlayKind::ContextMenu,
            Self::GlobalMenu(_) => ClientShellOverlayKind::GlobalMenu,
            Self::Settings(_) => ClientShellOverlayKind::Settings,
        }
    }
}

#[derive(Debug)]
pub(super) enum PendingEndpointKind {
    Generic,
    ProductAnnouncementDismiss {
        version: String,
        id: String,
    },
    ReleaseNotesDismiss,
    PopupCommand,
    ReloadConfig,
    IntegrationList,
    IntegrationInstall,
    PrepareWorktreeCreate {
        workspace_id: String,
    },
    PrepareWorktreeOpen {
        workspace_id: String,
    },
    PrepareWorktreeRemove {
        workspace_id: String,
    },
    WorktreeCreate,
    WorktreeOpen,
    WorktreeRemove {
        forced: bool,
    },
    SelectionCopy {
        fallback: Option<ClientMessage>,
    },
    PaneScroll {
        pane_id: String,
        serial: u64,
    },
    WordSelection {
        pane_id: String,
        absolute_row: u32,
        col: u16,
        generation: u64,
    },
    PaneLinkActivate {
        pane_id: String,
        inner_rect: Rect,
        fallback_events: Vec<crossterm::event::MouseEvent>,
    },
    CopyMotion {
        pane_id: String,
        origin: crate::api::schema::PaneTextPoint,
        session_generation: u64,
    },
    CopySearch {
        pane_id: String,
        origin: crate::api::schema::PaneTextPoint,
        query: String,
        direction: crate::api::schema::PaneCopySearchDirection,
        repeat: bool,
        generation: u64,
        session_generation: u64,
    },
}

pub(super) struct PendingEndpointRequest {
    pub(super) boot_id: String,
    pub(super) method_name: String,
    pub(super) confirmation_workspace_id: Option<String>,
    pub(super) kind: PendingEndpointKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ClientEndpointNoticeKind {
    Unsupported,
    Rejected,
    Timeout,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ClientEndpointNoticeKey {
    pub(super) boot_id: String,
    pub(super) kind: ClientEndpointNoticeKind,
    pub(super) code: String,
}

pub(super) struct ClientVisibleEndpointNotice {
    pub(super) key: ClientEndpointNoticeKey,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) deadline: std::time::Instant,
}

pub(crate) struct ClientShellEndpointError {
    pub code: Option<String>,
    pub message: String,
}

pub(crate) enum ClientShellNotificationEffect {
    Sound {
        sound: crate::sound::Sound,
        agent: Option<String>,
    },
    Terminal {
        title: String,
        body: Option<String>,
    },
    System {
        title: String,
        body: Option<String>,
    },
}

pub(super) struct ClientPendingNotification {
    pub(super) event: SemanticNotification,
    pub(super) deadline: std::time::Instant,
    pub(super) validate_state: bool,
}

pub(super) struct ClientVisibleNotification {
    pub(super) event: SemanticNotification,
    pub(super) deadline: std::time::Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClientInputTarget {
    Pane(String),
    Popup(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClientInputContext {
    pub(super) mode: ClientShellMode,
    pub(super) overlay: Option<ClientShellOverlayKind>,
    pub(super) popup_terminal_id: Option<String>,
    pub(super) popup_pending: bool,
    pub(super) retained_selection: bool,
}

type ClientInputLeases = crate::input::InputLeaseTable<u8, ClientInputContext, ClientInputTarget>;

#[derive(Clone, Debug)]
pub(super) struct ClientPaneClick {
    pub(super) pane_id: String,
    pub(super) viewport_row: u16,
    pub(super) col: u16,
    pub(super) at: std::time::Instant,
}

impl ClientPaneClick {
    pub(super) fn is_double_click_for(&self, next: &Self) -> bool {
        self.pane_id == next.pane_id
            && next.at.duration_since(self.at) <= std::time::Duration::from_millis(350)
            && self.viewport_row.abs_diff(next.viewport_row) <= 1
            && self.col.abs_diff(next.col) <= 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientSelectionAutoscrollDirection {
    Up,
    Down,
}

#[derive(Clone, Debug)]
pub(super) struct ClientSelectionAutoscroll {
    pub(super) pane_id: String,
    pub(super) direction: ClientSelectionAutoscrollDirection,
    pub(super) last_mouse_column: u16,
    pub(super) last_mouse_row: u16,
    pub(super) inner_rect: Rect,
    pub(super) offset_from_bottom: usize,
    pub(super) max_offset_from_bottom: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientCopySelection {
    Character {
        anchor: crate::api::schema::PaneTextPoint,
    },
    Linewise {
        anchor_row: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClientCopySearchPrompt {
    pub(super) direction: crate::api::schema::PaneCopySearchDirection,
    pub(super) query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClientCopyOperation {
    Motion(crate::api::schema::PaneCopyMotion),
    Search {
        query: String,
        direction: crate::api::schema::PaneCopySearchDirection,
        repeat: bool,
    },
}

pub(super) struct ClientCopySearchResult {
    pub(super) content_revision: u64,
    pub(super) matches: Vec<crate::api::schema::PaneTextRange>,
    pub(super) total: u64,
    pub(super) current: Option<usize>,
    pub(super) current_global: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClientCopyModeState {
    pub(super) pane_id: String,
    pub(super) content_revision: u64,
    pub(super) geometry: (u16, u16),
    pub(super) cursor: crate::api::schema::PaneTextPoint,
    pub(super) offset_from_bottom: usize,
    pub(super) max_offset_from_bottom: usize,
    pub(super) entry_offset_from_bottom: usize,
    pub(super) selection: Option<ClientCopySelection>,
    pub(super) search_prompt: Option<ClientCopySearchPrompt>,
    pub(super) search_query: String,
    pub(super) search_direction: Option<crate::api::schema::PaneCopySearchDirection>,
    pub(super) search_matches: Vec<crate::api::schema::PaneTextRange>,
    pub(super) search_total: u64,
    pub(super) search_current: Option<usize>,
    pub(super) search_current_global: Option<u64>,
    pub(super) search_generation: u64,
    pub(super) copy_after_search: bool,
}

pub(crate) struct ClientShellState {
    pub(super) config: ClientShellConfig,
    pub(super) snapshot: Option<Box<ClientShellSnapshot>>,
    pub(super) pane_surface: Option<PaneSurfaceFrame>,
    pub(super) graphics: crate::kitty_graphics::surface::ClientState,
    pub(super) graphics_cell_size: crate::kitty_graphics::HostCellSize,
    pub(super) popup_terminal_id: Option<String>,
    pub(super) sidebar_collapsed: bool,
    pub(super) sidebar_collapsed_manual: bool,
    pub(super) sidebar_width: u16,
    pub(super) sidebar_width_manual: bool,
    pub(super) sidebar_section_split: f32,
    pub(super) sidebar_section_split_manual: bool,
    pub(super) agent_panel_sort_manual: bool,
    pub(super) last_sidebar_divider_click: Option<std::time::Instant>,
    pub(super) chrome_drag: Option<ClientChromeDrag>,
    pub(super) workspace_press: Option<ClientWorkspacePress>,
    pub(super) tab_press: Option<ClientTabPress>,
    pub(super) collapsed_groups: HashSet<String>,
    pub(super) workspace_scroll: usize,
    pub(super) agent_scroll: usize,
    pub(super) tab_scroll: usize,
    pub(super) mobile_switcher_scroll: usize,
    pub(super) reveal_mobile_workspace: bool,
    pub(super) mobile_switcher_suspended: bool,
    pub(super) reveal_focused_tab: bool,
    pub(super) last_tab_bar_width: Option<u16>,
    pub(super) last_composed_size: Option<(u16, u16)>,
    pub(super) hits: ShellHitMap,
    pub(super) mode: ClientShellMode,
    pub(super) navigate_workspace_id: Option<String>,
    pub(super) overlay: Option<ClientShellOverlay>,
    pub(super) previous_pane_id: Option<String>,
    pub(super) pane_mouse_gesture: Option<ClientPaneMouseGesture>,
    pub(super) url_click_consumes_until_up: bool,
    pub(super) replaying_url_click: bool,
    pub(super) selection: Option<crate::selection::Selection<String>>,
    pub(super) last_pane_click: Option<ClientPaneClick>,
    pub(super) selection_autoscroll: Option<ClientSelectionAutoscroll>,
    pub(super) selection_autoscroll_deadline: Option<std::time::Instant>,
    pub(super) selection_highlight_clear_deadline: Option<std::time::Instant>,
    pub(super) pending_word_selection: Option<u64>,
    pub(super) word_selection_generation: u64,
    pub(super) copy_mode: Option<ClientCopyModeState>,
    pub(super) copy_session_generation: u64,
    pub(super) copy_operation_in_flight: bool,
    pub(super) copy_operation_queue: VecDeque<ClientCopyOperation>,
    pub(super) copy_input_queue: VecDeque<crate::input::TerminalKey>,
    pub(super) next_scroll_serial: u64,
    pub(super) pane_scroll_in_flight: HashMap<String, u64>,
    pub(super) pane_scroll_queued: HashMap<String, usize>,
    pub(super) pane_scroll_targets: HashMap<String, usize>,
    pub(super) copy_feedback: Option<crate::app::state::CopyFeedback>,
    pub(super) copy_feedback_deadline: Option<std::time::Instant>,
    pub(super) host_mouse_pixels: Option<crate::input::mouse::HostPixels>,
    pub(super) input_leases: ClientInputLeases,
    pub(super) popup_pending: bool,
    pub(super) popup_pending_deadline: Option<std::time::Instant>,
    pub(super) next_request_id: u64,
    /// Methods advertised by this endpoint. `None` is used only by local tests
    /// and legacy construction paths; negotiated endpoint connections always
    /// install an explicit set.
    pub(super) endpoint_methods: Option<HashSet<String>>,
    pub(super) pending_requests: HashMap<String, PendingEndpointRequest>,
    pub(super) pending_integration_installs: usize,
    pub(super) pending_notifications: Vec<ClientPendingNotification>,
    pub(super) visible_notification: Option<ClientVisibleNotification>,
    pub(super) endpoint_notice_seen: HashSet<ClientEndpointNoticeKey>,
    pub(super) visible_endpoint_notice: Option<ClientVisibleEndpointNotice>,
    pub(super) outer_focused: Option<bool>,
    pub(super) ascii_input_source_active: bool,
    pub(super) pending_input_source_changes: Vec<bool>,
    pub(super) host_appearance: Option<crate::terminal_theme::HostAppearance>,
    pub(super) host_appearance_explicit: bool,
    pub(super) local_config_diagnostic: Option<String>,
    pub(super) config_diagnostic: Option<String>,
    pub(super) endpoint_error: Option<String>,
    pub(super) dismissed_product_announcement: Option<(String, String)>,
}

pub(super) fn product_announcement_state(
    announcement: &crate::protocol::ClientShellProductAnnouncement,
) -> crate::app::state::ProductAnnouncementState {
    crate::app::state::ProductAnnouncementState {
        version: announcement.version.clone(),
        id: announcement.id.clone(),
        title: announcement.title.clone(),
        body: announcement.body.clone(),
        scroll: 0,
        preview: announcement.preview,
    }
}

pub(super) fn release_notes_state(
    notes: &crate::protocol::ClientShellReleaseNotes,
) -> crate::app::state::ReleaseNotesState {
    crate::app::state::ReleaseNotesState {
        version: notes.version.clone(),
        body: notes.body.clone(),
        scroll: 0,
        preview: notes.preview,
    }
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceEntry {
    pub(super) index: usize,
    pub(super) indented: bool,
    pub(super) last_child: bool,
}

impl ClientShellState {
    pub(crate) fn new(mut config: ClientShellConfig) -> Self {
        let preferences = config.preferences.clone();
        let local_config_diagnostic = config.startup_config_diagnostic.take();
        let overlay = config
            .startup_onboarding
            .then_some(ClientShellOverlay::Onboarding);
        let sidebar_collapsed = preferences
            .sidebar_collapsed
            .unwrap_or(config.sidebar_start_collapsed);
        let (min_width, max_width) = crate::config::validated_sidebar_bounds(
            config.sidebar_min_width,
            config.sidebar_max_width,
        )
        .unwrap_or((18, 36));
        let sidebar_width = preferences
            .sidebar_width
            .unwrap_or(config.sidebar_width)
            .clamp(min_width, max_width);
        let sidebar_section_split = preferences
            .sidebar_section_split
            .filter(|split| split.is_finite())
            .map(|split| split.clamp(0.1, 0.9))
            .unwrap_or(0.5);
        if let Some(sort) = preferences.agent_panel_sort {
            config.agent_panel_sort = sort;
        }
        Self {
            config,
            snapshot: None,
            pane_surface: None,
            graphics: crate::kitty_graphics::surface::ClientState::default(),
            graphics_cell_size: crate::kitty_graphics::HostCellSize {
                width_px: 1,
                height_px: 1,
            },
            popup_terminal_id: None,
            sidebar_collapsed,
            sidebar_collapsed_manual: preferences.sidebar_collapsed.is_some(),
            sidebar_width,
            sidebar_width_manual: preferences.sidebar_width.is_some(),
            sidebar_section_split,
            sidebar_section_split_manual: preferences.sidebar_section_split.is_some(),
            agent_panel_sort_manual: preferences.agent_panel_sort.is_some(),
            last_sidebar_divider_click: None,
            chrome_drag: None,
            workspace_press: None,
            tab_press: None,
            collapsed_groups: preferences.collapsed_groups.into_iter().collect(),
            workspace_scroll: 0,
            agent_scroll: 0,
            tab_scroll: 0,
            mobile_switcher_scroll: 0,
            reveal_mobile_workspace: false,
            mobile_switcher_suspended: false,
            reveal_focused_tab: true,
            last_tab_bar_width: None,
            last_composed_size: None,
            hits: ShellHitMap::default(),
            mode: ClientShellMode::Terminal,
            navigate_workspace_id: None,
            overlay,
            previous_pane_id: None,
            pane_mouse_gesture: None,
            url_click_consumes_until_up: false,
            replaying_url_click: false,
            selection: None,
            last_pane_click: None,
            selection_autoscroll: None,
            selection_autoscroll_deadline: None,
            selection_highlight_clear_deadline: None,
            pending_word_selection: None,
            word_selection_generation: 0,
            copy_mode: None,
            copy_session_generation: 0,
            copy_operation_in_flight: false,
            copy_operation_queue: VecDeque::new(),
            copy_input_queue: VecDeque::new(),
            next_scroll_serial: 0,
            pane_scroll_in_flight: HashMap::new(),
            pane_scroll_queued: HashMap::new(),
            pane_scroll_targets: HashMap::new(),
            copy_feedback: None,
            copy_feedback_deadline: None,
            host_mouse_pixels: None,
            input_leases: ClientInputLeases::default(),
            popup_pending: false,
            popup_pending_deadline: None,
            next_request_id: 1,
            endpoint_methods: None,
            pending_requests: HashMap::new(),
            pending_integration_installs: 0,
            pending_notifications: Vec::new(),
            visible_notification: None,
            endpoint_notice_seen: HashSet::new(),
            visible_endpoint_notice: None,
            outer_focused: None,
            ascii_input_source_active: false,
            pending_input_source_changes: Vec::new(),
            host_appearance: None,
            host_appearance_explicit: false,
            config_diagnostic: local_config_diagnostic.clone(),
            local_config_diagnostic,
            endpoint_error: None,
            dismissed_product_announcement: None,
        }
    }

    pub(crate) fn set_endpoint_methods(&mut self, methods: Option<Vec<String>>) {
        self.endpoint_methods = methods.map(|methods| methods.into_iter().collect());
    }

    pub(super) fn supports_endpoint_method(&self, method: &crate::api::schema::Method) -> bool {
        self.endpoint_methods
            .as_ref()
            .is_none_or(|methods| methods.contains(crate::api::api_method_name(method)))
    }

    fn focused_tab_count(&self) -> usize {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return 0;
        };
        snapshot
            .tabs
            .iter()
            .filter(|tab| {
                Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref()
            })
            .count()
    }

    pub(super) fn resume_mobile_switcher_if_ready(&mut self) -> bool {
        if !self.mobile_switcher_suspended || self.overlay.is_some() {
            return false;
        }
        self.mobile_switcher_suspended = false;
        if self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_workspace_id.as_ref())
            .is_some()
        {
            self.mode = self.copy_or_terminal_mode();
            self.navigate_workspace_id = None;
        } else {
            self.mode = ClientShellMode::Navigate;
        }
        true
    }

    pub(super) fn mobile_layout_active(&self) -> bool {
        self.last_composed_size
            .is_some_and(|(cols, rows)| !self.layout(cols, rows).mobile_header.is_empty())
    }

    pub(super) fn navigation_workspace_entries(
        &self,
        snapshot: &ClientShellSnapshot,
    ) -> Vec<WorkspaceEntry> {
        if self.mobile_layout_active() {
            render::workspace_entries(snapshot, &HashSet::new())
        } else {
            render::workspace_entries(snapshot, &self.collapsed_groups)
        }
    }

    pub(super) fn reveal_workspace(&mut self, workspace_id: &str) {
        if self
            .hits
            .workspaces
            .iter()
            .any(|hit| hit.workspace_id == workspace_id)
        {
            return;
        }
        let target = self.snapshot.as_deref().and_then(|snapshot| {
            self.navigation_workspace_entries(snapshot)
                .iter()
                .position(|entry| snapshot.workspaces[entry.index].workspace_id == workspace_id)
        });
        if let Some(target) = target {
            self.workspace_scroll = target.min(self.hits.workspace_max_scroll);
        }
    }

    pub(super) fn layout(&self, cols: u16, rows: u16) -> ClientShellLayout {
        self.config.layout(
            cols,
            rows,
            self.sidebar_collapsed,
            self.focused_tab_count(),
            self.sidebar_width,
        )
    }

    pub(crate) fn surface_size(&self, cols: u16, rows: u16) -> ClientSurfaceSize {
        let surface = self.layout(cols, rows).pane_surface;
        ClientSurfaceSize {
            cols: surface.width.max(1),
            rows: surface.height.max(1),
        }
    }

    pub(crate) fn set_snapshot(&mut self, mut snapshot: Box<ClientShellSnapshot>) {
        snapshot
            .commands
            .retain(|command| command.action != crate::protocol::ClientShellCommandAction::Unknown);
        if self.snapshot.as_ref().is_some_and(|current| {
            current.boot_id == snapshot.boot_id && snapshot.revision < current.revision
        }) {
            return;
        }
        self.graphics.set_scope(&snapshot.boot_id);
        let command_bindings_changed = self.snapshot.as_ref().is_none_or(|current| {
            current.commands.len() != snapshot.commands.len()
                || current
                    .commands
                    .iter()
                    .zip(&snapshot.commands)
                    .any(|(left, right)| {
                        left.binding_labels != right.binding_labels || left.action != right.action
                    })
        });
        let endpoint_profile_changed = self.snapshot.as_ref().is_none_or(|current| {
            current.server_keybindings_toml != snapshot.server_keybindings_toml
        });
        let snapshot_keybindings_changed = match self.config.keybinding_source {
            ClientShellKeybindingSource::Local => self
                .snapshot
                .as_ref()
                .is_none_or(|current| current.commands != snapshot.commands),
            ClientShellKeybindingSource::Endpoint => {
                endpoint_profile_changed
                    || self
                        .snapshot
                        .as_ref()
                        .is_none_or(|current| current.commands != snapshot.commands)
            }
            ClientShellKeybindingSource::RemoteLocal => false,
        };
        let active_keymap_changed = match self.config.keybinding_source {
            ClientShellKeybindingSource::Local => command_bindings_changed,
            ClientShellKeybindingSource::Endpoint => {
                endpoint_profile_changed || command_bindings_changed
            }
            ClientShellKeybindingSource::RemoteLocal => false,
        };
        self.config_diagnostic = super::config::merged_config_diagnostic(
            self.local_config_diagnostic.as_deref(),
            snapshot.config_diagnostic.as_deref(),
        );
        let boot_changed = self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.boot_id != snapshot.boot_id);
        if boot_changed
            || self
                .pane_surface
                .as_ref()
                .is_none_or(|surface| surface.projection_revision != snapshot.revision)
        {
            self.hits = ShellHitMap::default();
        }
        if boot_changed {
            self.pane_surface = None;
            self.input_leases = ClientInputLeases::default();
            self.popup_terminal_id = None;
            self.chrome_drag = None;
            self.workspace_press = None;
            self.tab_press = None;
            self.workspace_scroll = 0;
            self.agent_scroll = 0;
            self.tab_scroll = 0;
            self.mobile_switcher_scroll = 0;
            self.reveal_mobile_workspace = false;
            self.mobile_switcher_suspended = false;
            self.reveal_focused_tab = true;
            self.last_tab_bar_width = None;
            self.last_composed_size = None;
            self.pending_requests.clear();
            self.pane_scroll_in_flight.clear();
            self.pane_scroll_queued.clear();
            self.pane_scroll_targets.clear();
            self.popup_pending = false;
            self.popup_pending_deadline = None;
            self.pending_integration_installs = 0;
            self.pending_notifications.clear();
            self.visible_notification = None;
            self.endpoint_notice_seen.clear();
            self.visible_endpoint_notice = None;
            self.endpoint_error = None;
            self.navigate_workspace_id = None;
            self.overlay = self
                .config
                .startup_onboarding
                .then_some(ClientShellOverlay::Onboarding);
            self.previous_pane_id = None;
            self.pane_mouse_gesture = None;
            self.url_click_consumes_until_up = false;
            self.replaying_url_click = false;
            self.selection = None;
            self.last_pane_click = None;
            self.selection_autoscroll = None;
            self.selection_autoscroll_deadline = None;
            self.selection_highlight_clear_deadline = None;
            self.pending_word_selection = None;
            self.copy_mode = None;
            self.reset_copy_pipeline();
            self.copy_feedback = None;
            self.copy_feedback_deadline = None;
            self.host_mouse_pixels = None;
            self.dismissed_product_announcement = None;
        } else if let Some(previous) = self
            .snapshot
            .as_deref()
            .and_then(|current| current.focused_pane_id.as_ref())
            .filter(|previous| Some(previous.as_str()) != snapshot.focused_pane_id.as_deref())
        {
            self.previous_pane_id = Some(previous.clone());
        }
        if snapshot_keybindings_changed {
            if let Err(err) = self.config.apply_snapshot_keybindings(
                snapshot.server_keybindings_toml.as_deref(),
                &snapshot.commands,
            ) {
                self.endpoint_error = Some(err);
            } else if active_keymap_changed
                && matches!(
                    self.mode,
                    ClientShellMode::Prefix | ClientShellMode::Navigate | ClientShellMode::Resize
                )
            {
                self.mode = ClientShellMode::Terminal;
            }
        }
        let tab_layout_changed = self.snapshot.as_deref().is_none_or(|current| {
            current.tabs.len() != snapshot.tabs.len()
                || current
                    .tabs
                    .iter()
                    .zip(&snapshot.tabs)
                    .any(|(left, right)| {
                        left.tab_id != right.tab_id
                            || left.workspace_id != right.workspace_id
                            || left.label != right.label
                            || left.zoomed != right.zoomed
                    })
                || render::tab_bar_status_width(current) != render::tab_bar_status_width(&snapshot)
        });
        if tab_layout_changed
            || self
                .snapshot
                .as_deref()
                .and_then(|current| current.focused_tab_id.as_deref())
                != snapshot.focused_tab_id.as_deref()
        {
            self.reveal_focused_tab = true;
        }
        if self.selection.as_ref().is_some_and(|selection| {
            snapshot.focused_pane_id.as_deref() != Some(selection.pane_id.as_str())
                || !snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == selection.pane_id)
        }) {
            self.selection = None;
            self.selection_autoscroll = None;
            self.selection_autoscroll_deadline = None;
            self.selection_highlight_clear_deadline = None;
            self.pending_word_selection = None;
            self.last_pane_click = None;
        }
        if let Some(copy_pane_id) = self
            .copy_mode
            .as_ref()
            .map(|copy_mode| copy_mode.pane_id.clone())
        {
            let pane_exists = snapshot
                .panes
                .iter()
                .any(|pane| pane.pane_id == copy_pane_id);
            let pane_focused = snapshot.focused_pane_id.as_deref() == Some(copy_pane_id.as_str());
            if !pane_exists {
                self.copy_mode = None;
                self.reset_copy_pipeline();
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.pane_id == copy_pane_id)
                {
                    self.selection = None;
                    self.stop_selection_autoscroll();
                    self.selection_highlight_clear_deadline = None;
                }
                if self.mode == ClientShellMode::Copy {
                    self.mode = ClientShellMode::Terminal;
                }
            } else if pane_focused {
                if self.mode == ClientShellMode::Terminal {
                    self.mode = ClientShellMode::Copy;
                }
                if self.selection.is_none() {
                    self.sync_copy_selection();
                }
            } else {
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.pane_id == copy_pane_id)
                {
                    self.selection = None;
                    self.stop_selection_autoscroll();
                    self.selection_highlight_clear_deadline = None;
                }
                if self.mode == ClientShellMode::Copy {
                    self.mode = ClientShellMode::Terminal;
                }
            }
        }
        if self.mode == ClientShellMode::Navigate
            && self.navigate_workspace_id.as_ref().is_none_or(|selected| {
                !snapshot
                    .workspaces
                    .iter()
                    .any(|workspace| &workspace.workspace_id == selected)
            })
        {
            self.navigate_workspace_id = snapshot.focused_workspace_id.clone();
            self.reveal_mobile_workspace = self.mobile_layout_active();
        }
        let pane_exists =
            |pane_id: &String| snapshot.panes.iter().any(|pane| &pane.pane_id == pane_id);
        self.pane_scroll_in_flight
            .retain(|pane_id, _| pane_exists(pane_id));
        self.pane_scroll_queued
            .retain(|pane_id, _| pane_exists(pane_id));
        self.pane_scroll_targets
            .retain(|pane_id, _| pane_exists(pane_id));

        if !self.config.startup_onboarding {
            match snapshot.product_announcement.as_ref() {
                Some(announcement) => {
                    let key = (announcement.version.clone(), announcement.id.clone());
                    let already_open = matches!(
                        self.overlay.as_ref(),
                        Some(ClientShellOverlay::ProductAnnouncement(current))
                            if current.version == announcement.version && current.id == announcement.id
                    );
                    let may_open = self.overlay.is_none()
                        || matches!(
                            self.overlay.as_ref(),
                            Some(ClientShellOverlay::ProductAnnouncement(_))
                        );
                    if self.dismissed_product_announcement.as_ref() != Some(&key)
                        && may_open
                        && !already_open
                    {
                        self.overlay = Some(ClientShellOverlay::ProductAnnouncement(
                            product_announcement_state(announcement),
                        ));
                    }
                }
                None if matches!(
                    self.overlay.as_ref(),
                    Some(ClientShellOverlay::ProductAnnouncement(_))
                ) =>
                {
                    self.overlay = None;
                    self.chrome_drag = None;
                    self.dismissed_product_announcement = None;
                }
                None => {
                    self.dismissed_product_announcement = None;
                }
            }
        }
        if let Some(ClientShellOverlay::ReleaseNotes(current)) = self.overlay.as_ref() {
            match snapshot.release_notes.as_ref() {
                Some(notes)
                    if current.version != notes.version
                        || current.body != notes.body
                        || current.preview != notes.preview =>
                {
                    self.overlay =
                        Some(ClientShellOverlay::ReleaseNotes(release_notes_state(notes)));
                    self.chrome_drag = None;
                }
                None => {
                    self.overlay = None;
                    self.chrome_drag = None;
                }
                Some(_) => {}
            }
        }
        self.snapshot = Some(snapshot);
        self.resume_mobile_switcher_if_ready();
        self.reconcile_input_source();
    }

    pub(crate) fn set_pane_surface(&mut self, mut surface: PaneSurfaceFrame) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        if surface.boot_id != snapshot.boot_id
            || surface.projection_revision < snapshot.revision
            || self.pane_surface.as_ref().is_some_and(|current| {
                current.boot_id == surface.boot_id
                    && (surface.projection_revision < current.projection_revision
                        || (surface.projection_revision == current.projection_revision
                            && surface.surface_revision < current.surface_revision))
            })
        {
            return;
        }
        if self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.revision != surface.projection_revision)
        {
            self.hits = ShellHitMap::default();
        }
        let previous_popup = self.popup_terminal_id.clone();
        let next_popup = surface
            .popup
            .as_deref()
            .map(|popup| popup.terminal_id.clone());
        if previous_popup != next_popup {
            if next_popup.is_some() && matches!(self.overlay, Some(ClientShellOverlay::Settings(_)))
            {
                self.cancel_settings_overlay();
            }
            if let Some(terminal_id) = previous_popup.as_ref() {
                self.input_leases
                    .remove_target(&ClientInputTarget::Popup(terminal_id.clone()));
            }
            self.mode = ClientShellMode::Terminal;
            self.navigate_workspace_id = None;
            if !matches!(
                self.overlay.as_ref(),
                Some(ClientShellOverlay::Onboarding | ClientShellOverlay::ProductAnnouncement(_))
            ) {
                self.overlay = self
                    .config
                    .startup_onboarding
                    .then_some(ClientShellOverlay::Onboarding);
            }
            self.selection = None;
            self.last_pane_click = None;
            self.selection_autoscroll = None;
            self.selection_autoscroll_deadline = None;
            self.selection_highlight_clear_deadline = None;
            self.pending_word_selection = None;
            self.copy_mode = None;
            self.reset_copy_pipeline();
            self.chrome_drag = None;
            self.workspace_press = None;
            self.tab_press = None;
            if self.pane_mouse_gesture.as_ref().is_some_and(|gesture| {
                gesture.hit.popup && previous_popup.as_deref() == Some(gesture.hit.pane_id.as_str())
            }) {
                self.pane_mouse_gesture = None;
            }
            self.hits.popup = None;
            self.endpoint_error = None;
        }
        if next_popup.is_some() {
            self.popup_pending = false;
            self.popup_pending_deadline = None;
        }
        let selection_content_changed = self.selection.as_ref().is_some_and(|selection| {
            let previous_revision = self.pane_surface.as_ref().and_then(|previous| {
                previous
                    .panes
                    .iter()
                    .find(|pane| pane.pane_id == selection.pane_id)
                    .map(|pane| pane.content_revision)
            });
            let next_revision = surface
                .panes
                .iter()
                .find(|pane| pane.pane_id == selection.pane_id)
                .map(|pane| pane.content_revision);
            previous_revision.is_some()
                && next_revision.is_some()
                && previous_revision != next_revision
        });
        if selection_content_changed {
            self.selection = None;
            self.stop_selection_autoscroll();
            self.selection_highlight_clear_deadline = None;
        }
        for pane in &surface.panes {
            let Some(target) = self.pane_scroll_targets.get(&pane.pane_id).copied() else {
                continue;
            };
            let Some(scroll) = pane.scroll else {
                continue;
            };
            let target =
                target.min(usize::try_from(scroll.max_offset_from_bottom).unwrap_or(usize::MAX));
            if usize::try_from(scroll.offset_from_bottom).unwrap_or(usize::MAX) == target {
                self.pane_scroll_targets.remove(&pane.pane_id);
            }
        }
        let mut invalidated_copy_pane = None;
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            if let Some(pane) = surface
                .panes
                .iter()
                .find(|pane| pane.pane_id == copy_mode.pane_id)
            {
                let geometry = (pane.inner_rect.width, pane.inner_rect.height);
                if copy_mode.content_revision != pane.content_revision
                    || copy_mode.geometry != geometry
                {
                    copy_mode.content_revision = pane.content_revision;
                    copy_mode.geometry = geometry;
                    copy_mode.selection = None;
                    copy_mode.search_matches.clear();
                    copy_mode.search_total = 0;
                    copy_mode.search_current = None;
                    copy_mode.search_current_global = None;
                    copy_mode.search_generation = copy_mode.search_generation.saturating_add(1);
                    copy_mode.copy_after_search = false;
                    invalidated_copy_pane = Some(copy_mode.pane_id.clone());
                }
                if let Some(scroll) = pane.scroll {
                    let actual_offset =
                        usize::try_from(scroll.offset_from_bottom).unwrap_or(usize::MAX);
                    if !self.pane_scroll_targets.contains_key(&pane.pane_id) {
                        copy_mode.offset_from_bottom = actual_offset;
                    }
                    copy_mode.max_offset_from_bottom =
                        usize::try_from(scroll.max_offset_from_bottom).unwrap_or(usize::MAX);
                }
            }
        }
        if invalidated_copy_pane.as_ref().is_some_and(|pane_id| {
            self.selection
                .as_ref()
                .is_some_and(|selection| &selection.pane_id == pane_id)
        }) {
            self.selection = None;
            self.stop_selection_autoscroll();
            self.selection_highlight_clear_deadline = None;
        }
        self.popup_terminal_id = next_popup;
        self.graphics
            .set_scene(std::mem::take(&mut surface.graphics));
        self.pane_surface = Some(surface);
        self.resume_mobile_switcher_if_ready();
        self.reconcile_input_source();
    }

    pub(crate) fn tick_popup_pending(&mut self, now: std::time::Instant) {
        if self
            .popup_pending_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.popup_pending = false;
            self.popup_pending_deadline = None;
        }
    }

    pub(crate) fn show_copy_feedback(&mut self, now: std::time::Instant) -> bool {
        if !self.config.clipboard_toast_enabled {
            return false;
        }
        self.copy_feedback = Some(crate::app::state::CopyFeedback {
            message: "copied to clipboard".to_owned(),
        });
        self.copy_feedback_deadline = Some(now + std::time::Duration::from_secs(2));
        true
    }

    pub(crate) fn tick_copy_feedback(&mut self, now: std::time::Instant) -> bool {
        let mut repaint = false;
        if self
            .copy_feedback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.copy_feedback = None;
            self.copy_feedback_deadline = None;
            repaint = true;
        }
        if self
            .selection_highlight_clear_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.selection = None;
            self.selection_highlight_clear_deadline = None;
            repaint = true;
        }
        repaint
    }

    pub(crate) fn timer_delay(&self, now: std::time::Instant) -> std::time::Duration {
        let default = std::time::Duration::from_millis(100);
        self.selection_autoscroll_deadline
            .map(|deadline| deadline.saturating_duration_since(now).min(default))
            .unwrap_or(default)
    }

    pub(crate) fn invalidate_pane_surface(&mut self) {
        self.pane_surface = None;
        self.hits = ShellHitMap::default();
        self.host_mouse_pixels = None;
    }

    fn wants_ascii_input(&self) -> bool {
        if let Some(overlay) = self.overlay.as_ref() {
            return matches!(
                overlay,
                ClientShellOverlay::ConfirmClose(_)
                    | ClientShellOverlay::Help(_)
                    | ClientShellOverlay::Navigator(_)
                    | ClientShellOverlay::WorktreeRemove(_)
                    | ClientShellOverlay::ContextMenu(_)
                    | ClientShellOverlay::GlobalMenu(_)
            );
        }
        matches!(
            self.mode,
            ClientShellMode::Prefix
                | ClientShellMode::Navigate
                | ClientShellMode::Resize
                | ClientShellMode::Copy
        )
    }

    pub(crate) fn reconcile_input_source(&mut self) {
        // Keep the platform restore token while another window has focus. Restoring
        // through a global key injection is only safe after this client regains focus.
        if self.outer_focused == Some(false) {
            return;
        }
        let desired = self.config.switch_ascii_input_source_in_prefix && self.wants_ascii_input();
        if desired != self.ascii_input_source_active {
            self.ascii_input_source_active = desired;
            self.pending_input_source_changes.push(desired);
        }
    }

    pub(crate) fn take_input_source_changes(&mut self) -> Vec<bool> {
        std::mem::take(&mut self.pending_input_source_changes)
    }
}
