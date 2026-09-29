//! Input handling — translates crossterm key/mouse events into state mutations.

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tracing::warn;

use crate::app::{
    state::{ClientInputOwner, DockInputOwner, InputOwner, ServerInputOwner, SurfaceInputOwner},
    PaneClickState,
};
use crate::input::TerminalKey;
#[cfg(test)]
use ratatui::layout::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbarClickTarget {
    Thumb { grab_row_offset: u16 },
    Track { offset_from_bottom: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

const WORKSPACE_DRAG_THRESHOLD: u16 = 1;
const TAB_DRAG_THRESHOLD: u16 = 1;

fn modified_url_click_modifier() -> KeyModifiers {
    KeyModifiers::CONTROL
}

#[cfg(test)]
#[test]
fn modified_url_click_modifier_matches_terminal_mouse_reporting() {
    assert_eq!(modified_url_click_modifier(), KeyModifiers::CONTROL);
}

mod board;
mod clipboard;
mod copy_mode;
mod dock;
mod lease;
mod modal;
mod mouse;
mod navigate;
mod notepad;
mod overlays;
mod selection;
mod settings;
mod sidebar;
mod terminal;

pub(crate) use self::modal::open_rename_pod;

#[cfg(test)]
pub(crate) use self::modal::open_new_tab_dialog;
#[cfg(test)]
pub(crate) use self::navigate::{
    action_for_key_for_test, non_indexed_navigation_actions_for_test, BindingDispatch,
};
pub(crate) use self::notepad::NotepadRequest;
pub(crate) use self::sidebar::SidebarWorkGroupKeyAction;
pub(crate) use self::{
    lease::{ConsumedInputLease, ForwardedInputLease, InputLeaseKey, InputLeaseTable, RepeatPlan},
    modal::{
        handle_git_menu_key, handle_global_menu_key, handle_keybind_help_key, handle_navigator_key,
        insert_keybind_help_query_text, insert_navigator_search_text, insert_rename_input_text,
        open_new_workspace_dialog,
    },
    navigate::{
        terminal_direct_indexed_navigation_action, terminal_direct_non_indexed_navigation_action,
        ActionContext, NavigateAction,
    },
    settings::open_settings_at,
};
use self::{
    modal::{
        modal_action_from_key, ModalAction, ONBOARDING_WELCOME_ACTIONS, RELEASE_NOTES_ACTIONS,
    },
    mouse::MouseAction,
};
use super::state::{AppState, Mode};
use super::App;

impl AppState {
    pub(super) fn home_dismiss_picker(&mut self) {
        if let Some(home) = self.home.as_mut() {
            home.picker = None;
            home.browse = None;
        }
    }

    pub(super) fn home_focus_prompt(&mut self) {
        self.home_dismiss_picker();
        if let Some(home) = self.home.as_mut() {
            home.focus = Some(crate::app::home::HomeFocus::Prompt);
        }
    }

    pub(super) fn home_focus_reply(&mut self) {
        self.home_dismiss_picker();
        if let Some(home) = self.home.as_mut() {
            home.focus = Some(crate::app::home::HomeFocus::Reply);
        }
    }

    pub(super) fn home_focus_picker(&mut self, picker: crate::app::home::HomePicker) {
        self.home_dismiss_picker();
        if let Some(home) = self.home.as_mut() {
            home.focus = Some(match picker {
                crate::app::home::HomePicker::Agent => crate::app::home::HomeFocus::Agent,
                crate::app::home::HomePicker::Model => crate::app::home::HomeFocus::Model,
                crate::app::home::HomePicker::Effort => crate::app::home::HomeFocus::Effort,
                crate::app::home::HomePicker::Access => crate::app::home::HomeFocus::Access,
                crate::app::home::HomePicker::Context => crate::app::home::HomeFocus::Context,
                crate::app::home::HomePicker::Project => crate::app::home::HomeFocus::Project,
                crate::app::home::HomePicker::Repo => crate::app::home::HomeFocus::Repo,
                crate::app::home::HomePicker::Directory => crate::app::home::HomeFocus::Directory,
                crate::app::home::HomePicker::Machine => crate::app::home::HomeFocus::Machine,
                crate::app::home::HomePicker::Workspace => crate::app::home::HomeFocus::Workspace,
                crate::app::home::HomePicker::Ref => crate::app::home::HomeFocus::Ref,
                crate::app::home::HomePicker::Target => crate::app::home::HomeFocus::Target,
            });
        }
        self.home_open_picker(picker);
    }

    pub(super) fn home_move_composer_focus(&mut self, backwards: bool, queue_empty: bool) {
        self.home_dismiss_picker();
        if let Some(home) = self.home.as_mut() {
            if home.focus.is_none() {
                home.focus = Some(if queue_empty {
                    crate::app::home::HomeFocus::Prompt
                } else {
                    crate::app::home::HomeFocus::Reply
                });
            } else {
                home.move_focus(backwards);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Pull request writes that share the confirm-then-run path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PullRequestAction {
    Approve,
    Merge,
    Close,
}

impl App {
    pub(crate) fn open_fleet_host_from_input(&mut self, name: &str, focus_agent: Option<&str>) {
        self.state.clear_home();
        self.open_fleet_host_focused(name, focus_agent);
    }

    #[cfg(test)]
    pub(super) async fn handle_key(
        &mut self,
        key: TerminalKey,
    ) -> Option<super::TerminalInputTarget> {
        let prompt_visible = self.state.pomodoro.prompt.is_some();
        self.handle_key_with_pomodoro_prompt_visibility(key, prompt_visible)
            .await
    }

    #[cfg(test)]
    pub(super) async fn handle_key_with_pomodoro_prompt_visibility(
        &mut self,
        key: TerminalKey,
        prompt_visible: bool,
    ) -> Option<super::TerminalInputTarget> {
        let owner = self.state.input_owner_with_pomodoro(prompt_visible);
        self.handle_key_for_input_owner(key, owner).await
    }

    pub(super) async fn handle_key_for_input_owner(
        &mut self,
        key: TerminalKey,
        owner: InputOwner,
    ) -> Option<super::TerminalInputTarget> {
        self.state.clear_hovered_control();
        let target = self.handle_key_inner_for_input_owner(key, owner).await;
        // Every keyboard path that can enter a probed settings section runs
        // through here, so the probes start once from one place.
        self.start_requested_tool_probes();
        target
    }

    #[cfg(test)]
    async fn handle_key_inner(&mut self, key: TerminalKey) -> Option<super::TerminalInputTarget> {
        let prompt_visible = self.state.pomodoro.prompt.is_some();
        self.handle_key_inner_with_pomodoro_prompt_visibility(key, prompt_visible)
            .await
    }

    #[cfg(test)]
    async fn handle_key_inner_with_pomodoro_prompt_visibility(
        &mut self,
        key: TerminalKey,
        prompt_visible: bool,
    ) -> Option<super::TerminalInputTarget> {
        let owner = self.state.input_owner_with_pomodoro(prompt_visible);
        self.handle_key_inner_for_input_owner(key, owner).await
    }

    async fn handle_key_inner_for_input_owner(
        &mut self,
        key: TerminalKey,
        owner: InputOwner,
    ) -> Option<super::TerminalInputTarget> {
        let key_event = key.as_key_event();
        if self.state.board_return.is_some()
            && key_event.code == KeyCode::Esc
            && key_event.modifiers.is_empty()
            && matches!(owner, InputOwner::Pane | InputOwner::Popup)
        {
            self.state.board_view = self.state.board_return.take();
            return None;
        }
        if self.handle_window_cycle_menu_key(key_event, owner) {
            return None;
        }
        if self.handle_sidebar_areas_menu_key(key_event, owner) {
            return None;
        }
        if self.paste_clipboard_shortcut_for_input_owner(
            owner,
            &key_event,
            crate::platform::read_clipboard_text,
        ) {
            return None;
        }
        match owner {
            InputOwner::Pomodoro => {
                self.intercept_notepad_key_with_prompt_visibility(&key, true);
            }
            InputOwner::Client(owner) => match owner {
                ClientInputOwner::Overlay(overlay) => {
                    self.handle_client_overlay_key(overlay, key_event);
                }
                ClientInputOwner::SnoozeMenu => {
                    self.handle_sidebar_snooze_menu_key(key_event);
                }
                ClientInputOwner::SnoozeTime => {
                    self.handle_sidebar_snooze_time_key(key_event);
                }
                ClientInputOwner::SettledMenu | ClientInputOwner::SettledDeleteConfirm => {
                    self.handle_sidebar_settled_key(key_event);
                }
                ClientInputOwner::AgentPicker => self.handle_agent_picker_key(key_event),
                ClientInputOwner::SidebarGroupMenu => {
                    self.state.handle_sidebar_group_menu_key(key_event);
                }
                ClientInputOwner::SidebarFilterMenu => {
                    self.state.handle_sidebar_filter_menu_key(key_event);
                }
                ClientInputOwner::SidebarNewMenu => {
                    self.state.handle_sidebar_new_menu_key(key_event);
                }
                ClientInputOwner::SidebarNewThread => {
                    self.state.handle_sidebar_new_thread_key(key_event);
                }
                ClientInputOwner::SidebarProjectMenu => {
                    self.state.handle_sidebar_project_menu_key(key_event);
                }
                ClientInputOwner::SidebarObjectMenu => {
                    self.handle_sidebar_object_menu_key(key_event);
                }
                ClientInputOwner::SidebarSortMenu => {
                    self.state.handle_sidebar_sort_menu_key(key_event);
                }
                ClientInputOwner::SidebarSubgroupPicker => {
                    self.state.handle_sidebar_subgroup_picker_key(key_event);
                }
                ClientInputOwner::SidebarPodPicker => {
                    self.handle_sidebar_pod_picker_key(key_event);
                }
                ClientInputOwner::PrActionConfirmation => {
                    self.handle_pr_action_confirmation_key(key_event);
                }
                ClientInputOwner::DockSurfaceMenu => {
                    self.handle_dock_surface_menu_key(&key);
                }
            },
            InputOwner::AddProject | InputOwner::Surface(SurfaceInputOwner::Home) => {
                self.handle_home_key_event(key_event);
            }
            InputOwner::Server(owner) => match owner {
                ServerInputOwner::Onboarding => self.handle_onboarding_key(key_event),
                ServerInputOwner::ReleaseNotes => self.handle_release_notes_key(key_event),
                ServerInputOwner::ProductAnnouncement => {
                    self.handle_product_announcement_key(key_event)
                }
                ServerInputOwner::Navigate => self.handle_navigate_key(key),
                ServerInputOwner::Prefix => self.handle_prefix_key(key),
                ServerInputOwner::Copy => self.handle_copy_mode_key(key),
                ServerInputOwner::Resize => self.handle_resize_key_via_api(key),
                ServerInputOwner::GitMenu => handle_git_menu_key(&mut self.state, key_event),
                ServerInputOwner::AddAction => self.handle_add_action_key(key_event),
                ServerInputOwner::Settings => self.handle_settings_key(key_event),
                ServerInputOwner::GlobalMenu => handle_global_menu_key(&mut self.state, key_event),
                ServerInputOwner::KeybindHelp => handle_keybind_help_key(&mut self.state, key),
                ServerInputOwner::Navigator => {
                    handle_navigator_key(&mut self.state, &self.terminal_runtimes, key_event)
                }
                ServerInputOwner::CommandPalette => self.handle_command_palette_key(key_event),
                ServerInputOwner::WorkLinkPicker => self.handle_work_link_picker_key(key_event),
            },
            InputOwner::Popup | InputOwner::Pane => return self.handle_terminal_key(key).await,
            InputOwner::Surface(SurfaceInputOwner::Symphony) => {
                self.handle_symphony_key(key_event);
            }
            InputOwner::Surface(SurfaceInputOwner::LoopRunHistory) => {
                self.handle_loop_run_history_key(key_event);
            }
            InputOwner::Surface(SurfaceInputOwner::AloopRunLog) => {
                self.handle_aloop_run_detail_key(key_event);
            }
            InputOwner::Surface(SurfaceInputOwner::Usage) => {
                self.handle_usage_view_key(key_event);
            }
            InputOwner::Surface(SurfaceInputOwner::Work) => {
                self.handle_work_view_key(key_event);
            }
            InputOwner::Surface(SurfaceInputOwner::Board) => {
                self.handle_board_key(key_event);
            }
            InputOwner::Surface(SurfaceInputOwner::DockObjectPreview) => {
                if key_event.code == KeyCode::Esc && key_event.modifiers.is_empty() {
                    self.state.dock_object_preview = None;
                    self.state.dock_pr_focused = false;
                    self.state.dock_linear_focused = false;
                }
            }
            InputOwner::Surface(SurfaceInputOwner::Inbox) => {
                return self.handle_inbox_key(key).await;
            }
            InputOwner::Surface(SurfaceInputOwner::EditorPreview) => {}
            InputOwner::Notepad => {
                self.intercept_notepad_key_with_prompt_visibility(&key, false);
            }
            InputOwner::Dock(owner) => {
                if matches!(owner, DockInputOwner::Editor) {
                    return self.handle_terminal_key(key).await;
                }
                if self.handle_dock_chooser_key(&key) {
                    return None;
                }
                let consumed = match owner {
                    DockInputOwner::Home => self.handle_dock_home_key(&key),
                    DockInputOwner::PullRequest => self.handle_dock_pr_key(&key),
                    DockInputOwner::Linear => self.handle_dock_linear_key(&key),
                    DockInputOwner::Diff => self.handle_dock_diff_key(&key),
                    DockInputOwner::Files => self.handle_dock_files_key(&key),
                    DockInputOwner::Agents => self.handle_dock_agents_key(&key),
                    DockInputOwner::Hosts => self.handle_dock_hosts_key(&key),
                    DockInputOwner::Chooser | DockInputOwner::Editor => false,
                };
                if !consumed {
                    return self.handle_terminal_key(key).await;
                }
            }
            InputOwner::Sidebar => {
                if self.state.handle_sidebar_search_key(key_event) {
                    return None;
                }
                if self.handle_sidebar_object_menu_key(key_event) {
                    return None;
                }
                match self.state.handle_sidebar_work_group_key(key_event) {
                    sidebar::SidebarWorkGroupKeyAction::Ignored => {}
                    sidebar::SidebarWorkGroupKeyAction::Consumed => return None,
                    sidebar::SidebarWorkGroupKeyAction::Dispatch(plan) => {
                        self.dispatch_sidebar_work_group_plan(*plan);
                        return None;
                    }
                    sidebar::SidebarWorkGroupKeyAction::RenamePod(record) => {
                        modal::open_rename_pod(&mut self.state, record);
                        return None;
                    }
                    sidebar::SidebarWorkGroupKeyAction::DeletePod(record) => {
                        self.runtime_delete_pod(record);
                        return None;
                    }
                }
                if !self.handle_sidebar_session_action_key(key_event) {
                    self.handle_sidebar_settled_key(key_event);
                }
            }
            InputOwner::None => {}
        }
        None
    }

    pub(super) fn handle_client_overlay_key(
        &mut self,
        overlay: crate::app::state::ClientOverlay,
        key: KeyEvent,
    ) -> bool {
        match overlay {
            crate::app::state::ClientOverlay::RenameWorkspace
            | crate::app::state::ClientOverlay::RenameTab
            | crate::app::state::ClientOverlay::RenamePane => self.handle_rename_key_via_api(key),
            crate::app::state::ClientOverlay::NewLinkedWorktree => {
                self.handle_worktree_create_key(key)
            }
            crate::app::state::ClientOverlay::OpenExistingWorktree => {
                self.handle_worktree_open_key(key)
            }
            crate::app::state::ClientOverlay::ConfirmRemoveWorktree => {
                self.handle_worktree_remove_key(key)
            }
            crate::app::state::ClientOverlay::ConfirmClose => {
                self.handle_confirm_close_key_via_api(key)
            }
            crate::app::state::ClientOverlay::ContextMenu => {
                self.handle_context_menu_key_via_api(key)
            }
            crate::app::state::ClientOverlay::None => return false,
        }
        true
    }

    /// Card shortcuts are written uppercase, so the shift that produces them is
    /// part of the binding rather than a different chord.
    fn dock_shortcut_modifiers(modifiers: KeyModifiers) -> bool {
        modifiers.is_empty() || modifiers == KeyModifiers::SHIFT
    }

    /// Keys of the surface chooser: the card-grid shortcuts of an empty dock,
    /// the open `+` menu, and the one keypress that restores a maximised dock.
    fn handle_dock_chooser_key(&mut self, key: &TerminalKey) -> bool {
        if self.state.effective_interaction_mode() != Mode::Terminal || self.state.dock_collapsed {
            return false;
        }
        let event = key.as_key_event();

        // The uppercase card shortcuts remain available while a non-terminal
        // dock surface owns focus. Requiring Shift here preserves Home's
        // lowercase action keys and never steals input from the editor PTY.
        if (self.state.dock_home_focused || self.state.dock_diff_focused)
            && event.modifiers == KeyModifiers::SHIFT
        {
            if let KeyCode::Char(character) = event.code {
                if let Some(surface) = crate::app::DockSurface::from_shortcut(character) {
                    return self.state.activate_dock_surface(surface);
                }
            }
        }

        // A maximised dock leaves no pane to type into, so one Esc gives the
        // main area back. The editor is the exception: its keys belong to the
        // PTY, and the ⤢ click restores it instead.
        if self.state.dock_maximized
            && !self.state.dock_editor_focused
            && event.code == KeyCode::Esc
            && event.modifiers.is_empty()
        {
            self.state.dock_maximized = false;
            return true;
        }

        let dock_surface_focused = self.state.dock_home_focused
            || self.state.dock_files_focused
            || self.state.dock_agents_focused
            || self.state.dock_hosts_focused
            || self.state.dock_chooser_focused;
        if dock_surface_focused && self.state.dock_tab.is_some() {
            if let KeyCode::Char(character) = event.code {
                if Self::dock_shortcut_modifiers(event.modifiers) {
                    if let Some(surface) = crate::app::DockSurface::from_shortcut(character) {
                        return self.state.activate_dock_surface(surface);
                    }
                }
            }
        }

        if self.state.dock_tab.is_some() || !self.state.dock_chooser_focused {
            return false;
        }
        match event.code {
            KeyCode::Char(character) if Self::dock_shortcut_modifiers(event.modifiers) => {
                match crate::app::DockSurface::from_card_shortcut(character) {
                    Some(surface) => {
                        self.state.activate_dock_surface(surface);
                        true
                    }
                    None => false,
                }
            }
            KeyCode::Esc => {
                self.state.dock_chooser_focused = false;
                true
            }
            _ => false,
        }
    }

    fn handle_dock_home_key(&mut self, key: &TerminalKey) -> bool {
        if self.state.dock_collapsed
            || self.state.dock_tab != Some(crate::app::DockSurface::Home)
            || !self.state.dock_home_focused
        {
            return false;
        }

        let event = key.as_key_event();

        // A staged write owns the keyboard until it is confirmed or dropped, so
        // nothing can leave herdr as a side effect of ordinary navigation.
        if self.handle_pending_dock_write_key(event) {
            return true;
        }
        let navigate = &self.state.keybinds.navigate;

        // While a comment is being typed the keys belong to the draft, not to
        // the action shortcuts.
        if let Some(draft) = self.state.dock_comment_draft.as_mut() {
            match event.code {
                KeyCode::Esc => {
                    self.state.dock_comment_draft = None;
                }
                KeyCode::Backspace => {
                    draft.pop();
                }
                KeyCode::Enter if event.modifiers.is_empty() => {
                    self.stage_dock_comment();
                }
                KeyCode::Char(character)
                    if event.modifiers.is_empty()
                        || event.modifiers == crossterm::event::KeyModifiers::SHIFT =>
                {
                    draft.push(character);
                }
                _ => {}
            }
            return true;
        }

        if event.code == KeyCode::Esc && event.modifiers.is_empty() {
            self.state.dock_home_focused = false;
            self.state.dock_scroll = 0;
            return true;
        }

        if event.modifiers.is_empty() {
            match event.code {
                KeyCode::Char('r') => {
                    self.state.dock_comment_draft = Some(String::new());
                    self.state.dock_write_notice = None;
                    return true;
                }
                KeyCode::Char('a') => {
                    return self.stage_pull_request_action(PullRequestAction::Approve)
                }
                KeyCode::Char('m') => {
                    return self.stage_pull_request_action(PullRequestAction::Merge)
                }
                KeyCode::Char('x') => {
                    return self.stage_pull_request_action(PullRequestAction::Close)
                }
                _ => {}
            }
        }
        // Arrows only. `pane_left`/`pane_right`/`pane_up`/`pane_down` default to
        // bare `h`/`j`/`k`/`l`, and the dock is focused over a live shell, so
        // consuming those would swallow ordinary typing into the pane.
        if navigate.workspace_up.matches_direct_key(key)
            || matches!(event.code, KeyCode::Up | KeyCode::Left) && event.modifiers.is_empty()
        {
            self.state.move_dock_home_selection(-1);
            return true;
        }
        if navigate.workspace_down.matches_direct_key(key)
            || matches!(event.code, KeyCode::Down | KeyCode::Right) && event.modifiers.is_empty()
        {
            self.state.move_dock_home_selection(1);
            return true;
        }

        if event.code == KeyCode::Enter && event.modifiers.is_empty() {
            self.state.jump_to_dock_home_selection();
            return true;
        }
        false
    }

    fn handle_dock_diff_key(&mut self, key: &TerminalKey) -> bool {
        if self.state.effective_interaction_mode() != Mode::Terminal
            || self.state.dock_collapsed
            || self.state.dock_tab != Some(crate::app::DockSurface::Diff)
            || !self.state.dock_diff_focused
        {
            return false;
        }
        let event = key.as_key_event();
        if !event.modifiers.is_empty() {
            return false;
        }
        let file_count = self
            .state
            .dock_diff_active_key
            .as_ref()
            .and_then(|key| self.state.dock_diff_cache.get(key))
            .map_or(0, |entry| entry.files.len());
        match event.code {
            KeyCode::Char('w') => self.state.toggle_dock_diff_whitespace(),
            KeyCode::Down | KeyCode::Char('j') => {
                if file_count > 0 {
                    self.state.dock_diff_selected =
                        (self.state.dock_diff_selected + 1).min(file_count - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.dock_diff_selected = self.state.dock_diff_selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                self.state.toggle_selected_dock_diff_file();
            }
            KeyCode::Esc => self.state.dock_diff_focused = false,
            _ => return false,
        }
        true
    }

    pub(super) fn handle_dock_agents_key(&mut self, key: &TerminalKey) -> bool {
        if self.state.effective_interaction_mode() != Mode::Terminal
            || self.state.dock_collapsed
            || self.state.dock_tab != Some(crate::app::DockSurface::Agents)
            || !self.state.dock_agents_focused
        {
            return false;
        }
        let event = key.as_key_event();
        if !event.modifiers.is_empty() {
            return false;
        }
        match event.code {
            KeyCode::Down => self.state.move_dock_agents_selection(1),
            KeyCode::Up => self.state.move_dock_agents_selection(-1),
            KeyCode::Enter => {
                let path = self
                    .state
                    .selected_dock_agent_observation()
                    .and_then(|observation| observation.transcript_path.clone());
                if let Some(path) = path {
                    self.open_file_in_dock_editor(path);
                }
            }
            KeyCode::Esc => self.state.dock_agents_focused = false,
            _ => return false,
        }
        true
    }

    pub(super) fn handle_dock_hosts_key(&mut self, key: &TerminalKey) -> bool {
        if self.state.effective_interaction_mode() != Mode::Terminal
            || self.state.dock_collapsed
            || self.state.dock_tab != Some(crate::app::DockSurface::Hosts)
            || !self.state.dock_hosts_focused
        {
            return false;
        }
        let event = key.as_key_event();
        if !event.modifiers.is_empty() {
            return false;
        }
        match event.code {
            KeyCode::Down => self.state.move_dock_hosts_selection(1),
            KeyCode::Up => self.state.move_dock_hosts_selection(-1),
            KeyCode::Enter => {
                if let Some(name) = self
                    .state
                    .selected_fleet_host()
                    .map(|host| host.name.clone())
                {
                    self.open_fleet_host(&name);
                }
            }
            KeyCode::Esc => self.state.dock_hosts_focused = false,
            _ => return false,
        }
        true
    }

    /// Stage a pull request action for confirmation. Returns false when the
    /// selection is not a pull request, so the key falls through unchanged.
    fn stage_pull_request_action(&mut self, action: PullRequestAction) -> bool {
        if self.state.dock_home_section != crate::app::state::DockHomeSection::Prs {
            return false;
        }
        let Some(row) = self.state.dock_home_selected_row() else {
            return false;
        };
        let (Some(number), repo) = (row.key.pr_number, row.key.repo.clone()) else {
            return false;
        };
        if repo.is_empty() {
            return false;
        }
        self.state.dock_pending_write = Some(Self::pull_request_write(action, repo, number));
        self.state.dock_write_notice = None;
        true
    }

    fn pull_request_write(
        action: PullRequestAction,
        repo: String,
        number: u64,
    ) -> crate::work_index::WorkItemWrite {
        match action {
            PullRequestAction::Approve => {
                crate::work_index::WorkItemWrite::ApprovePullRequest { repo, number }
            }
            PullRequestAction::Merge => {
                crate::work_index::WorkItemWrite::MergePullRequest { repo, number }
            }
            PullRequestAction::Close => {
                crate::work_index::WorkItemWrite::ClosePullRequest { repo, number }
            }
        }
    }

    /// Turn the typed draft into a staged comment on whatever is selected.
    fn stage_dock_comment(&mut self) {
        let Some(body) = self
            .state
            .dock_comment_draft
            .as_ref()
            .map(|draft| draft.trim().to_string())
            .filter(|draft| !draft.is_empty())
        else {
            return;
        };
        let write = match self.state.dock_home_section {
            crate::app::state::DockHomeSection::Prs => {
                self.state.dock_home_selected_row().and_then(|row| {
                    let number = row.key.pr_number?;
                    (!row.key.repo.is_empty()).then(|| {
                        crate::work_index::WorkItemWrite::CommentOnPullRequest {
                            repo: row.key.repo.clone(),
                            number,
                            body: body.clone(),
                        }
                    })
                })
            }
            crate::app::state::DockHomeSection::Tickets => {
                let projection = self.state.dock_home_projection();
                self.state
                    .dock_home_selected_ticket_index(&projection)
                    .and_then(|index| projection.ticket_rows.get(index))
                    .map(|row| crate::work_index::WorkItemWrite::CommentOnTicket {
                        identifier: row.ticket.identifier.clone(),
                        body: body.clone(),
                    })
            }
            crate::app::state::DockHomeSection::XPolls => None,
        };
        let Some(write) = write else {
            self.state.dock_write_notice = Some("nothing selected to comment on".to_string());
            return;
        };
        self.state.dock_pending_write = Some(write);
    }

    /// Run the staged write. The draft survives a failure: text a human typed is
    /// not thrown away because a network call did not land.
    fn run_pending_dock_write(&mut self) {
        let Some(write) = self.state.dock_pending_write.take() else {
            return;
        };
        let gh = self.work_index_gh_program();
        let linearis = self.work_index_linearis_program();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        match crate::work_index::run_work_item_write(&write, &gh, &linearis, deadline) {
            Ok(message) => {
                self.state.dock_write_notice = Some(message);
                self.state.dock_comment_draft = None;
                // Drop the cached detail so the next refresh shows the result.
                if let Some(key) = write.target() {
                    self.state.work_item_detail_cache.remove(&key);
                }
            }
            Err(message) => self.state.dock_write_notice = Some(message),
        }
    }

    /// Shared confirmation gate for every work-item write staged from the dock
    /// or sidebar. The staging control can never execute its own write.
    fn handle_pending_dock_write_key(&mut self, event: KeyEvent) -> bool {
        if self.state.dock_pending_write.is_none() {
            return false;
        }
        match event.code {
            KeyCode::Char('y' | 'Y') if event.modifiers.is_empty() => {
                self.run_pending_dock_write();
            }
            KeyCode::Esc | KeyCode::Char('n' | 'N') if event.modifiers.is_empty() => {
                self.state.dock_pending_write = None;
                self.state.dock_write_notice = Some("cancelled".to_string());
            }
            _ => {}
        }
        true
    }

    /// Dock home is attach-local presentation layered over a focused pane, so
    /// the headless input path must offer it keys before choosing a pane target.
    pub(crate) fn handle_dock_home_key_headless(&mut self, key: &TerminalKey) -> bool {
        self.state.popup_pane.is_none() && self.handle_dock_home_key(key)
    }

    pub(crate) fn handle_dock_diff_key_headless(&mut self, key: &TerminalKey) -> bool {
        self.state.popup_pane.is_none() && self.handle_dock_diff_key(key)
    }

    #[cfg(test)]
    pub(crate) fn handle_dock_agents_key_headless(&mut self, key: &TerminalKey) -> bool {
        self.state.popup_pane.is_none() && self.handle_dock_agents_key(key)
    }

    pub(crate) fn handle_dock_pr_key_headless(&mut self, key: &TerminalKey) -> bool {
        self.state.popup_pane.is_none()
            && (self.handle_pr_action_confirmation_key(key.as_key_event())
                || self.handle_dock_pr_key(key))
    }

    pub(crate) fn handle_dock_linear_key_headless(&mut self, key: &TerminalKey) -> bool {
        self.state.popup_pane.is_none() && self.handle_dock_linear_key(key)
    }

    /// Same for the surface chooser: an empty dock owns its card shortcuts
    /// before a pane sees them.
    pub(crate) fn handle_dock_chooser_key_headless(&mut self, key: &TerminalKey) -> bool {
        self.state.popup_pane.is_none() && self.handle_dock_chooser_key(key)
    }

    /// Home owns the full key stream so navigation never leaks into a pane.
    /// Headless mirror.
    pub(crate) fn handle_home_key_headless(&mut self, key: KeyEvent) -> bool {
        self.state.home.is_some() && self.handle_home_key_event(key)
    }

    fn handle_home_text_commit(&mut self, text: &str) -> bool {
        let Some(home) = self.state.home.as_mut() else {
            return false;
        };
        if let Some(project) = home.add_project.as_mut() {
            project.push_text(text);
            self.start_home_github_refresh_if_requested();
            return true;
        }
        if matches!(
            home.picker,
            Some(crate::app::home::HomePicker::Directory | crate::app::home::HomePicker::Ref)
        ) {
            for character in text.chars() {
                match home.picker {
                    Some(crate::app::home::HomePicker::Directory) => {
                        home.directory_filter.push(character)
                    }
                    Some(crate::app::home::HomePicker::Ref) => home.ref_filter.push(character),
                    _ => {}
                }
            }
            return true;
        }
        match home.focus {
            Some(crate::app::home::HomeFocus::Prompt) => home.prompt.push_str(text),
            Some(crate::app::home::HomeFocus::Reply) => {
                home.reply.push_str(text);
                home.reply_error = None;
            }
            _ => {}
        }
        self.start_home_ref_refresh_if_requested();
        true
    }

    /// Handle a key while home is open. Returns whether home consumed it.
    fn handle_home_key_event(&mut self, event: KeyEvent) -> bool {
        if self.state.home.is_none() {
            return false;
        }

        if self.state.add_project_active() {
            self.state.handle_add_project_key(event);
            self.start_home_github_refresh_if_requested();
            return true;
        }

        if let Some(picker) = self.state.home.as_ref().and_then(|home| home.picker) {
            match event.code {
                // The path input owns tab, enter and escape: they complete,
                // accept and leave the input rather than moving the composer.
                KeyCode::Tab if event.modifiers.is_empty() && self.state.home_browse_active() => {
                    self.state.home_browse_complete();
                }
                KeyCode::Enter if event.modifiers.is_empty() && self.state.home_browse_active() => {
                    self.state.home_browse_accept();
                }
                KeyCode::Esc if self.state.home_browse_active() => {
                    self.state.home_browse_cancel();
                }
                KeyCode::Tab if event.modifiers.is_empty() => {
                    let queue_empty = self.state.home_attention_agents().is_empty();
                    self.state.home_move_composer_focus(false, queue_empty);
                }
                KeyCode::BackTab => {
                    let queue_empty = self.state.home_attention_agents().is_empty();
                    self.state.home_move_composer_focus(true, queue_empty);
                }
                KeyCode::Up if event.modifiers.is_empty() => {
                    self.state.home_move_picker(-1);
                }
                KeyCode::Down if event.modifiers.is_empty() => {
                    self.state.home_move_picker(1);
                }
                KeyCode::Char('k')
                    if event.modifiers.is_empty()
                        && !matches!(
                            picker,
                            crate::app::home::HomePicker::Directory
                                | crate::app::home::HomePicker::Ref
                        ) =>
                {
                    self.state.home_move_picker(-1);
                }
                KeyCode::Char('j')
                    if event.modifiers.is_empty()
                        && !matches!(
                            picker,
                            crate::app::home::HomePicker::Directory
                                | crate::app::home::HomePicker::Ref
                        ) =>
                {
                    self.state.home_move_picker(1);
                }
                KeyCode::Backspace
                    if event.modifiers.is_empty()
                        && matches!(
                            picker,
                            crate::app::home::HomePicker::Directory
                                | crate::app::home::HomePicker::Ref
                        ) =>
                {
                    self.state.home_pop_picker_filter();
                }
                KeyCode::Char(character)
                    if event.modifiers.is_empty()
                        && matches!(
                            picker,
                            crate::app::home::HomePicker::Directory
                                | crate::app::home::HomePicker::Ref
                        ) =>
                {
                    self.state.home_push_picker_filter(character);
                }
                KeyCode::Enter if event.modifiers.is_empty() => {
                    self.state.home_accept_picker();
                }
                KeyCode::Esc => {
                    self.state.home_dismiss_picker();
                }
                _ => {}
            }
            return true;
        }

        let focus = self.state.home.as_ref().and_then(|home| home.focus);
        let queue = self.state.home_attention_agents();

        match event.code {
            KeyCode::Tab if event.modifiers.is_empty() => {
                self.state.home_move_composer_focus(false, queue.is_empty());
            }
            KeyCode::BackTab => {
                self.state.home_move_composer_focus(true, queue.is_empty());
            }
            KeyCode::Up | KeyCode::Char('k') if event.modifiers.is_empty() && focus.is_none() => {
                if let Some(home) = self.state.home.as_mut() {
                    home.select_prev(&queue);
                }
            }
            KeyCode::Down | KeyCode::Char('j') if event.modifiers.is_empty() && focus.is_none() => {
                if let Some(home) = self.state.home.as_mut() {
                    home.select_next(&queue);
                }
            }
            // Shift+Enter is the newline, so plain Enter stays the submit key
            // the composer already trained the operator on.
            KeyCode::Enter
                if event.modifiers == KeyModifiers::SHIFT
                    && focus == Some(crate::app::home::HomeFocus::Prompt) =>
            {
                if let Some(home) = self.state.home.as_mut() {
                    home.append_prompt('\n');
                }
            }
            KeyCode::Enter if event.modifiers.is_empty() => match focus {
                None => {
                    self.state.jump_to_selected_home_agent(&queue);
                }
                Some(crate::app::home::HomeFocus::Reply) => {
                    self.reply_to_selected_home_agent();
                }
                Some(crate::app::home::HomeFocus::Prompt) => {
                    crate::logging::home_enter();
                    self.dispatch_home_prompt();
                }
                Some(focus) => {
                    if let Some(picker) = crate::app::home::HomePicker::for_focus(focus) {
                        self.state.home_open_picker(picker);
                    }
                }
            },
            KeyCode::Esc => {
                let close_home = self
                    .state
                    .home
                    .as_mut()
                    .is_some_and(|home| home.close_composer_or_home());
                if close_home {
                    self.state.clear_home();
                    self.state.set_server_mode(Mode::Terminal);
                }
            }
            KeyCode::Char('d')
                if event.modifiers.is_empty() && focus.is_none() && !queue.is_empty() =>
            {
                self.state.jump_to_selected_home_agent(&queue);
            }
            KeyCode::Char('n')
                if event.modifiers.is_empty() && focus.is_none() && !queue.is_empty() =>
            {
                self.state.home_focus_prompt();
            }
            KeyCode::Backspace
                if event.modifiers.is_empty()
                    && focus == Some(crate::app::home::HomeFocus::Prompt) =>
            {
                if let Some(home) = self.state.home.as_mut() {
                    home.backspace_prompt();
                }
            }
            KeyCode::Char(character)
                if event.modifiers.is_empty()
                    && focus == Some(crate::app::home::HomeFocus::Prompt) =>
            {
                if let Some(home) = self.state.home.as_mut() {
                    home.append_prompt(character);
                }
            }
            KeyCode::Backspace
                if event.modifiers.is_empty()
                    && focus == Some(crate::app::home::HomeFocus::Reply) =>
            {
                if let Some(home) = self.state.home.as_mut() {
                    home.backspace_reply();
                }
            }
            KeyCode::Char(character)
                if event.modifiers.is_empty()
                    && focus == Some(crate::app::home::HomeFocus::Reply) =>
            {
                if let Some(home) = self.state.home.as_mut() {
                    home.append_reply(character);
                }
            }
            _ => {}
        }
        self.start_home_ref_refresh_if_requested();
        true
    }

    fn dispatch_home_prompt(&mut self) {
        if self
            .state
            .home
            .as_ref()
            .is_some_and(|home| home.pending_dispatch.is_some())
        {
            return;
        }
        let Some(plan) = self
            .state
            .home
            .as_ref()
            .and_then(|home| home.dispatch_plan().ok())
        else {
            let message = self
                .state
                .home
                .as_ref()
                .and_then(|home| home.dispatch_plan().err())
                .unwrap_or_else(|| "dispatch failed".into());
            crate::logging::home_dispatch_failed(&message);
            let previous_toast = self.state.toast.clone();
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: crate::app::state::ToastKind::NeedsAttention,
                title: "dispatch failed".into(),
                context: message,
                position: None,
                target: None,
            });
            self.sync_toast_deadline(previous_toast);
            return;
        };

        let dispatch = self.dispatch_home_plan(plan);
        self.finish_home_dispatch(dispatch);
    }

    pub(super) fn dispatch_sidebar_work_group_plan(
        &mut self,
        plan: crate::app::home::HomeDispatchPlan,
    ) {
        let mut home = self.state.new_home_state();
        home.prompt = plan.prompt.clone();
        home.directory = plan.directory.clone();
        home.ref_directory = plan.directory.clone();
        home.workspace = plan.workspace.clone();
        home.selected_ref = plan.git_ref.clone();
        home.pr = plan.pr.clone();
        home.ticket = plan.ticket.clone();
        home.target = plan.target.clone();
        self.state.home = Some(home);
        self.state.inbox = None;
        let dispatch = self.dispatch_home_plan(plan);
        if dispatch.is_ok() {
            self.state.dock_object_preview = None;
        }
        self.finish_home_dispatch(dispatch);
    }

    fn dispatch_home_plan(
        &mut self,
        plan: crate::app::home::HomeDispatchPlan,
    ) -> Result<(), String> {
        crate::logging::home_dispatch_started(
            &format!("{:?}", plan.agent),
            plan.argv.first().map(String::as_str),
            &format!("{:?}", plan.workspace),
            &format!("{:?}", plan.target),
            &plan.directory,
        );

        if plan.remote.is_some() {
            return self.start_home_checkout(plan);
        }

        match plan.workspace {
            crate::app::home::HomeWorkspace::NewWorktree => self.start_home_worktree_add(plan),
            crate::app::home::HomeWorkspace::CurrentCheckout
                if plan
                    .git_ref
                    .as_ref()
                    .is_some_and(|git_ref| !git_ref.is_current()) =>
            {
                self.start_home_checkout(plan)
            }
            crate::app::home::HomeWorkspace::CurrentCheckout
            | crate::app::home::HomeWorkspace::PreviousWorktree(_) => self
                .dispatch_home_composer(plan)
                .map_err(|error| error.to_string()),
        }
    }

    fn finish_home_dispatch(&mut self, dispatch: Result<(), String>) {
        match dispatch {
            Ok(()) => {
                if self
                    .state
                    .home
                    .as_ref()
                    .is_none_or(|home| home.pending_dispatch.is_none())
                {
                    self.state.clear_home();
                    self.state.set_server_mode(Mode::Terminal);
                }
            }
            Err(error) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.dispatch_error = Some(error);
                }
            }
        }
    }

    pub(crate) fn handle_loop_run_history_key(&mut self, key: KeyEvent) -> bool {
        if self.state.loop_run_history_detail.is_none() {
            return false;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.state.clear_loop_run_history();
            self.state.set_server_mode(Mode::Terminal);
        }
        true
    }

    pub(crate) fn handle_aloop_run_detail_key(&mut self, key: KeyEvent) -> bool {
        if self.state.aloop_run_detail.is_none() {
            return false;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.state.clear_aloop_run_detail();
            self.state.set_server_mode(Mode::Terminal);
        }
        true
    }

    /// Inbox keys go to the blocked agent on screen, never to the focused pane.
    /// Only Esc and the defer key are the inbox's own; everything else is the
    /// operator answering the agent, which is the entire point of the mode.
    pub(crate) async fn handle_inbox_key(
        &mut self,
        key: crate::input::TerminalKey,
    ) -> Option<super::TerminalInputTarget> {
        let event = key.as_key_event();
        if event.code == KeyCode::Esc && event.modifiers.is_empty() {
            self.state.clear_inbox();
            self.state.set_server_mode(Mode::Terminal);
            return None;
        }
        let queue = self.state.blocked_agents();
        if event.code == KeyCode::Tab && event.modifiers.is_empty() {
            self.defer_current_inbox_agent(&queue);
            return None;
        }
        let target = super::TerminalInputTarget::new(self.inbox_target(&queue)?);
        self.forward_terminal_key_to_target(&target, key).await;
        Some(target)
    }

    /// Headless mirror. Returns whether the inbox consumed the key.
    pub(crate) fn handle_inbox_key_headless(&mut self, key: KeyEvent) -> bool {
        if self.state.inbox.is_none() {
            return false;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.state.clear_inbox();
            self.state.set_server_mode(Mode::Terminal);
            return true;
        }
        let queue = self.state.blocked_agents();
        if key.code == KeyCode::Tab && key.modifiers.is_empty() {
            self.defer_current_inbox_agent(&queue);
        }
        true
    }

    fn inbox_target(
        &self,
        queue: &[crate::app::inbox::BlockedAgent],
    ) -> Option<crate::terminal::TerminalId> {
        self.state
            .inbox
            .as_ref()?
            .current(queue)
            .map(|agent| agent.terminal_id.clone())
    }

    fn defer_current_inbox_agent(&mut self, queue: &[crate::app::inbox::BlockedAgent]) {
        let Some(pane_id) = self
            .state
            .inbox
            .as_ref()
            .and_then(|inbox| inbox.current(queue))
            .map(|agent| agent.pane_id)
        else {
            return;
        };
        if let Some(inbox) = self.state.inbox.as_mut() {
            inbox.defer(pane_id, queue);
        }
    }

    /// A status-bar button does exactly what its keybinding does, so the two
    /// affordances can never drift into meaning different things.
    fn activate_status_button(&mut self, action: crate::app::state::StatusButtonAction) {
        use crate::app::state::StatusButtonAction;
        match action {
            StatusButtonAction::Home => {
                self.state.toggle_home();
            }
            StatusButtonAction::Work => self.toggle_work_view(),
            StatusButtonAction::BlockedFilter => {
                self.state.blocked_filter = !self.state.blocked_filter;
                self.state.workspace_scroll = crate::ui::normalized_workspace_scroll(
                    &self.state,
                    self.state.view.sidebar_rect,
                    self.state.workspace_scroll,
                );
            }
            StatusButtonAction::Attention => {
                self.state.toggle_home();
            }
            StatusButtonAction::Dock => {
                self.state.dock_collapsed = !self.state.dock_collapsed;
            }
            StatusButtonAction::StatusDetail => {
                self.state.status_bar_expanded = !self.state.status_bar_expanded;
            }
        }
    }

    pub(crate) fn handle_symphony_key(&mut self, key: KeyEvent) -> bool {
        let Some(detail) = self.state.symphony_detail.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.state.clear_symphony();
                self.state.set_server_mode(Mode::Terminal);
            }
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                detail.selected = detail.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                detail.selected =
                    (detail.selected + 1).min(detail.snapshot.workflows.len().saturating_sub(1));
            }
            KeyCode::Enter if key.modifiers.is_empty() => self.open_selected_symphony_workflow(),
            _ => {}
        }
        true
    }

    pub(crate) fn toggle_work_view(&mut self) {
        self.toggle_work_projection(crate::app::state::WorkProjection::PullRequests);
    }

    pub(crate) fn toggle_ticket_view(&mut self) {
        self.toggle_work_projection(crate::app::state::WorkProjection::Tickets);
    }

    pub(crate) fn toggle_missive_view(&mut self) {
        self.toggle_work_projection(crate::app::state::WorkProjection::Missive);
    }

    fn toggle_work_projection(&mut self, projection: crate::app::state::WorkProjection) {
        self.state.clear_usage_view();
        self.state.board_view = None;
        if self
            .state
            .work_view
            .as_ref()
            .is_some_and(|view| view.projection == projection)
        {
            self.state.clear_work_view();
            return;
        }
        let enabled = self.work_index_config.enabled;
        let snapshot = enabled.then(|| self.work_index_snapshot.clone()).flatten();
        self.state.work_view = Some(crate::app::state::WorkViewState::new(enabled, snapshot));
        if let Some(view) = self.state.work_view.as_mut() {
            view.projection = projection;
            if projection == crate::app::state::WorkProjection::Tickets {
                view.ticket_layout = self.state.linear_default_layout;
            }
        }
        self.state.follow_view(match projection {
            crate::app::state::WorkProjection::PullRequests => {
                crate::app::state::SidebarGroupMode::RepoPr
            }
            crate::app::state::WorkProjection::Tickets => {
                crate::app::state::SidebarGroupMode::LinearTeam
            }
            crate::app::state::WorkProjection::Missive => {
                crate::app::state::SidebarGroupMode::Missive
            }
            crate::app::state::WorkProjection::Agents
            | crate::app::state::WorkProjection::ReviewQueue => {
                crate::app::state::SidebarGroupMode::Repo
            }
        });
        self.state.symphony_detail = None;
        self.state.inbox = None;
        self.state.clear_home();
        if self.state.work_view.is_some() && enabled {
            self.next_work_index_refresh = std::time::Instant::now();
            if let Some(view) = self.state.work_view.as_mut() {
                view.refreshing = true;
            }
        }
    }

    pub(crate) fn toggle_usage_view(&mut self) {
        self.state.toggle_usage_view();
        if self.state.usage_view.is_some() {
            self.start_usage_scan();
        }
    }

    pub(crate) fn handle_usage_view_key(&mut self, key: KeyEvent) -> bool {
        if self.state.usage_view.is_none() {
            return false;
        }
        use crate::app::state::{UsageBreakdown, UsageMetric, UsageRange};
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.state.clear_usage_view();
                self.state.set_server_mode(Mode::Terminal);
            }
            KeyCode::Char('c') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.metric = UsageMetric::Cost;
                }
            }
            KeyCode::Char('t') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.metric = UsageMetric::Tokens;
                }
            }
            KeyCode::Char('1') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.range = UsageRange::Hours24;
                }
            }
            KeyCode::Char('7') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.range = UsageRange::Days7;
                }
            }
            KeyCode::Char('3') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.range = UsageRange::Days30;
                }
            }
            KeyCode::Char('9') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.range = UsageRange::Days90;
                }
            }
            KeyCode::Char('m') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.breakdown = UsageBreakdown::Model;
                }
            }
            KeyCode::Char('d') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.breakdown = UsageBreakdown::Day;
                }
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                if let Some(view) = self.state.usage_view.as_mut() {
                    view.scanning = true;
                }
                self.start_usage_scan();
            }
            _ => self.dispatch_usage_passthrough_key(key),
        }
        true
    }

    /// Keys the usage screen does not own still reach the global direct
    /// keybindings and the prefix key, so tab switching keeps working while
    /// the screen is open.
    fn dispatch_usage_passthrough_key(&mut self, key: KeyEvent) {
        let key = TerminalKey::from(key);
        let action = terminal_direct_non_indexed_navigation_action(&self.state, &key)
            .or_else(|| terminal_direct_indexed_navigation_action(&self.state, &key));
        if let Some(action) = action {
            self.execute_tui_navigate_action(action, navigate::ActionContext::Direct);
        } else if self.state.is_prefix_key(&key) {
            self.state.set_server_mode(Mode::Prefix);
        }
    }

    fn activate_usage_hit_target(&mut self, target: crate::app::state::UsageHitTarget) {
        use crate::app::state::{UsageBreakdown, UsageHitTarget, UsageMetric, UsageRange};
        let Some(view) = self.state.usage_view.as_mut() else {
            return;
        };
        match target {
            UsageHitTarget::Cost => view.metric = UsageMetric::Cost,
            UsageHitTarget::Tokens => view.metric = UsageMetric::Tokens,
            UsageHitTarget::Hours24 => view.range = UsageRange::Hours24,
            UsageHitTarget::Days7 => view.range = UsageRange::Days7,
            UsageHitTarget::Days30 => view.range = UsageRange::Days30,
            UsageHitTarget::Days90 => view.range = UsageRange::Days90,
            UsageHitTarget::Model => view.breakdown = UsageBreakdown::Model,
            UsageHitTarget::Day => view.breakdown = UsageBreakdown::Day,
            UsageHitTarget::Rescan => {
                view.scanning = true;
                self.start_usage_scan();
            }
        }
    }

    pub(crate) fn handle_work_view_key(&mut self, key: KeyEvent) -> bool {
        let Some(state) = self.state.work_view.as_ref() else {
            return false;
        };
        let board_active = state.projection == crate::app::state::WorkProjection::Tickets
            && state.ticket_layout == crate::app::state::LinearViewLayout::Board;
        if board_active && state.board_detail_open && key.code == KeyCode::Esc {
            if let Some(state) = self.state.work_view.as_mut() {
                state.board_detail_open = false;
            }
            return true;
        }
        if state.pending_write.is_some() {
            match key.code {
                KeyCode::Char('y' | 'Y') if key.modifiers.is_empty() => {
                    self.run_pending_work_view_write();
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.pending_write = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        let selected_object_key = state.selected.clone().or_else(|| match state.projection {
            crate::app::state::WorkProjection::PullRequests => {
                self.visible_pr_view_keys().first().cloned()
            }
            crate::app::state::WorkProjection::Tickets => {
                self.visible_ticket_view_keys().first().cloned()
            }
            _ => None,
        });
        let selected_pr_key = (state.projection == crate::app::state::WorkProjection::PullRequests)
            .then(|| selected_object_key.clone())
            .flatten();
        if let Some((object_key, selected)) = selected_pr_key.as_ref().and_then(|object_key| {
            state
                .object_views
                .get(object_key)
                .and_then(|view| view.tab_picker)
                .map(|selected| (object_key.clone(), selected))
        }) {
            let count = crate::app::state::PrDetailTab::ALL.len();
            match key.code {
                KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                    if let Some(view) = self.state.work_view.as_mut() {
                        view.object_view_mut(object_key).tab_picker =
                            Some(selected.saturating_sub(1));
                    }
                }
                KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                    if let Some(view) = self.state.work_view.as_mut() {
                        view.object_view_mut(object_key).tab_picker =
                            Some((selected + 1).min(count.saturating_sub(1)));
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(view) = self.state.work_view.as_mut() {
                        let object = view.object_view_mut(object_key);
                        object.tab = crate::app::state::PrDetailTab::ALL[selected];
                        object.scroll = 0;
                        object.tab_picker = None;
                    }
                }
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(view) = self.state.work_view.as_mut() {
                        view.object_view_mut(object_key).tab_picker = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if state.reviewer_picker.is_some() {
            let collaborators = self
                .selected_pr_detail()
                .map(|(_, detail)| detail.collaborators.clone())
                .unwrap_or_default();
            let matches = self
                .state
                .work_view
                .as_ref()
                .and_then(|view| view.reviewer_picker.as_ref())
                .map(|picker| picker.filter.matches(&collaborators))
                .unwrap_or_default();
            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(view) = self.state.work_view.as_mut() {
                        view.reviewer_picker = None;
                    }
                }
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    if let Some(picker) = self
                        .state
                        .work_view
                        .as_mut()
                        .and_then(|view| view.reviewer_picker.as_mut())
                    {
                        picker.filter.pop();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                    if let Some(picker) = self
                        .state
                        .work_view
                        .as_mut()
                        .and_then(|view| view.reviewer_picker.as_mut())
                    {
                        picker.filter.move_selection(-1, matches.len());
                    }
                }
                KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                    if let Some(picker) = self
                        .state
                        .work_view
                        .as_mut()
                        .and_then(|view| view.reviewer_picker.as_mut())
                    {
                        picker.filter.move_selection(1, matches.len());
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    let selected = self
                        .state
                        .work_view
                        .as_ref()
                        .and_then(|view| view.reviewer_picker.as_ref())
                        .map(|picker| picker.filter.selected)
                        .unwrap_or_default();
                    if let Some((_, login)) = matches.get(selected) {
                        self.add_selected_pr_reviewer((*login).to_string());
                    }
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty()
                        || key.modifiers == crossterm::event::KeyModifiers::SHIFT =>
                {
                    if let Some(picker) = self
                        .state
                        .work_view
                        .as_mut()
                        .and_then(|view| view.reviewer_picker.as_mut())
                    {
                        picker.filter.push(character);
                    }
                }
                _ => {}
            }
            return true;
        }
        if state.ticket_comment_draft.is_some() {
            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_comment_draft = None;
                    }
                }
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    if let Some(draft) = self
                        .state
                        .work_view
                        .as_mut()
                        .and_then(|state| state.ticket_comment_draft.as_mut())
                    {
                        draft.pop();
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => self.stage_ticket_comment(),
                KeyCode::Char(character)
                    if key.modifiers.is_empty()
                        || key.modifiers == crossterm::event::KeyModifiers::SHIFT =>
                {
                    if let Some(draft) = self
                        .state
                        .work_view
                        .as_mut()
                        .and_then(|state| state.ticket_comment_draft.as_mut())
                    {
                        draft.push(character);
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some(choice) = state.ticket_start_menu {
            match key.code {
                KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_start_menu = Some(match choice {
                            crate::app::state::PrCheckoutChoice::CurrentCheckout => {
                                crate::app::state::PrCheckoutChoice::NewWorktree
                            }
                            crate::app::state::PrCheckoutChoice::NewWorktree => {
                                crate::app::state::PrCheckoutChoice::CurrentCheckout
                            }
                        });
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => self.open_selected_ticket_thread(),
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_start_menu = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some(choice) = state.missive_start_menu {
            match key.code {
                KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.missive_start_menu = Some(match choice {
                            crate::app::state::PrCheckoutChoice::CurrentCheckout => {
                                crate::app::state::PrCheckoutChoice::NewWorktree
                            }
                            crate::app::state::PrCheckoutChoice::NewWorktree => {
                                crate::app::state::PrCheckoutChoice::CurrentCheckout
                            }
                        });
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => self.open_selected_missive_thread(),
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.missive_start_menu = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some(choice) = state.ticket_transition_menu {
            match key.code {
                KeyCode::Up if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_transition_menu = Some(choice.move_by(-1));
                    }
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_transition_menu = Some(choice.move_by(1));
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => self.stage_ticket_transition(choice),
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_transition_menu = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some(mut menu) = state.ticket_more_menu {
            let entries = self
                .selected_ticket_action_context()
                .map(|context| crate::ui::ticket_actions::ticket_action_table(&context, menu.page))
                .unwrap_or_default();
            match key.code {
                KeyCode::Up if key.modifiers.is_empty() => {
                    menu.move_by(-1, entries.len());
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_more_menu = Some(menu);
                    }
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    menu.move_by(1, entries.len());
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_more_menu = Some(menu);
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(entry) = entries.get(menu.selected).filter(|entry| entry.enabled())
                    {
                        self.activate_ticket_action(entry.action);
                    }
                }
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_more_menu = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some(menu) = state.pr_action_menu {
            let actions = self.selected_pr_action_table();
            match key.code {
                KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        if let Some(menu) = state.pr_action_menu.as_mut() {
                            crate::ui::pr_actions::move_selection(menu, &actions, -1);
                        }
                    }
                }
                KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        if let Some(menu) = state.pr_action_menu.as_mut() {
                            crate::ui::pr_actions::move_selection(menu, &actions, 1);
                        }
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(action) = crate::ui::pr_actions::menu_actions(&actions)
                        .get(menu.selected)
                        .filter(|action| action.enabled())
                    {
                        let kind = action.kind;
                        if let Some(state) = self.state.work_view.as_mut() {
                            state.pr_action_menu = None;
                        }
                        self.activate_selected_pr_action(kind);
                    }
                }
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.pr_action_menu = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if state.checkout_menu.is_some() {
            match key.code {
                KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.checkout_menu = Some(match state.checkout_menu {
                            Some(crate::app::state::PrCheckoutChoice::CurrentCheckout) => {
                                crate::app::state::PrCheckoutChoice::NewWorktree
                            }
                            _ => crate::app::state::PrCheckoutChoice::CurrentCheckout,
                        });
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => self.open_selected_pr_checkout(),
                KeyCode::Esc if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.checkout_menu = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if state.search_focused {
            match key.code {
                KeyCode::Esc | KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.search_focused = false;
                    }
                }
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.search.pop();
                        state.selected = None;
                        state.selected_missive = None;
                        state.missive_detail_scroll = 0;
                    }
                }
                KeyCode::Char(character)
                    if key.modifiers.is_empty()
                        || key.modifiers == crossterm::event::KeyModifiers::SHIFT =>
                {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.search.push(character);
                        state.selected = None;
                        state.selected_missive = None;
                        state.missive_detail_scroll = 0;
                    }
                }
                _ => {}
            }
            return true;
        }
        if board_active && !state.board_detail_open {
            match key.code {
                KeyCode::Left if key.modifiers.is_empty() => {
                    self.move_ticket_board_column(-1);
                    return true;
                }
                KeyCode::Right if key.modifiers.is_empty() => {
                    self.move_ticket_board_column(1);
                    return true;
                }
                KeyCode::Up if key.modifiers.is_empty() => {
                    self.move_ticket_board_row(-1);
                    return true;
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    self.move_ticket_board_row(1);
                    return true;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.board_detail_open = state.selected.is_some();
                    }
                    return true;
                }
                KeyCode::Char('t') if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        if state.selected.is_some() {
                            state.ticket_transition_menu = Some(Default::default());
                        }
                    }
                    return true;
                }
                KeyCode::Char('v' | 'l') if key.modifiers.is_empty() => {
                    if let Some(state) = self.state.work_view.as_mut() {
                        state.ticket_layout = crate::app::state::LinearViewLayout::List;
                    }
                    return true;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.state.clear_work_view();
                self.state.set_server_mode(Mode::Terminal);
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.rotate(false);
                    state.hint = None;
                }
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.rotate(true);
                    state.hint = None;
                }
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                self.move_pr_view_selection(-1);
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.move_pr_view_selection(1);
            }
            KeyCode::PageUp if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Missive {
                        state.missive_detail_scroll = state.missive_detail_scroll.saturating_sub(5);
                    } else if let Some(object_key) = selected_object_key.clone() {
                        let view = state.object_view_mut(object_key);
                        view.scroll = view.scroll.saturating_sub(5);
                    }
                }
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Missive {
                        state.missive_detail_scroll = state.missive_detail_scroll.saturating_add(5);
                    } else if let Some(object_key) = selected_object_key.clone() {
                        let view = state.object_view_mut(object_key);
                        view.scroll = view.scroll.saturating_add(5);
                    }
                }
            }
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.search_focused = true;
                }
            }
            KeyCode::Char('s') if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Tickets {
                        state.ticket_sort = state.ticket_sort.next();
                    } else if state.projection == crate::app::state::WorkProjection::PullRequests {
                        state.sort = state.sort.next();
                    }
                    state.selected = None;
                    state.selected_missive = None;
                    state.missive_detail_scroll = 0;
                }
            }
            KeyCode::Char('f') if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Tickets {
                        state.ticket_open_only = !state.ticket_open_only;
                    } else {
                        state.open_only = !state.open_only;
                    }
                    state.selected = None;
                    state.selected_missive = None;
                    state.missive_detail_scroll = 0;
                }
            }
            KeyCode::Char('v' | 'b') if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Tickets {
                        state.ticket_layout = match state.ticket_layout {
                            crate::app::state::LinearViewLayout::List => {
                                crate::app::state::LinearViewLayout::Board
                            }
                            crate::app::state::LinearViewLayout::Board => {
                                crate::app::state::LinearViewLayout::List
                            }
                        };
                        state.board_detail_open = false;
                    }
                }
            }
            KeyCode::Tab if key.modifiers.is_empty() => {
                let detail_width = if self.state.view.terminal_area.width >= 72 {
                    self.state.view.terminal_area.width.saturating_mul(62) / 100
                } else {
                    self.state.view.terminal_area.width
                };
                if let (Some(object_key), Some(state)) =
                    (selected_pr_key.clone(), self.state.work_view.as_mut())
                {
                    let view = state.object_view_mut(object_key);
                    if detail_width < 60 {
                        view.scroll = 0;
                        view.tab_picker = crate::app::state::PrDetailTab::ALL
                            .iter()
                            .position(|tab| *tab == view.tab);
                    } else {
                        view.tab = view.tab.next();
                        view.scroll = 0;
                    }
                }
            }
            KeyCode::Char('c') if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Tickets {
                        state.ticket_start_menu = Some(Default::default());
                    } else if state.projection == crate::app::state::WorkProjection::Missive {
                        state.missive_start_menu = Some(Default::default());
                    } else {
                        state.checkout_menu = Some(Default::default());
                    }
                }
            }
            KeyCode::Char('t') if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Tickets {
                        state.ticket_transition_menu = Some(Default::default());
                    }
                }
            }
            KeyCode::Char('l') if key.modifiers.is_empty() => {
                if self.state.work_view.as_ref().is_some_and(|state| {
                    state.projection == crate::app::state::WorkProjection::Tickets
                }) {
                    self.stage_ticket_link_pr();
                } else {
                    self.activate_selected_pr_action(
                        crate::ui::work_list_detail::PrActionKind::Merge(
                            self.state.pr_merge_method,
                        ),
                    );
                }
            }
            KeyCode::Char('m') if key.modifiers.is_empty() => {
                if let Some(state) = self.state.work_view.as_mut() {
                    if state.projection == crate::app::state::WorkProjection::Tickets {
                        state.ticket_more_menu = Some(Default::default());
                    } else if state.projection == crate::app::state::WorkProjection::PullRequests {
                        state.pr_action_menu = Some(Default::default());
                    }
                }
            }
            KeyCode::Char('+')
                if key.modifiers.is_empty()
                    || key.modifiers == crossterm::event::KeyModifiers::SHIFT =>
            {
                if self.state.work_view.as_ref().is_some_and(|state| {
                    state.projection == crate::app::state::WorkProjection::PullRequests
                        && selected_pr_key.as_ref().is_some_and(|key| {
                            state.object_view(key).tab == crate::app::state::PrDetailTab::Overview
                        })
                }) {
                    self.open_selected_pr_reviewer_picker();
                }
            }
            KeyCode::Char('o') if key.modifiers.is_empty() => {
                if self.state.work_view.as_ref().is_some_and(|state| {
                    state.projection == crate::app::state::WorkProjection::Missive
                }) {
                    self.copy_selected_missive_url();
                }
            }
            // Alt digits fold sections, taken from the render's own order so a
            // view that hides a section never leaves a gap in the numbering.
            KeyCode::Char(digit @ '1'..='9')
                if key.modifiers == crossterm::event::KeyModifiers::ALT =>
            {
                let index = digit as usize - '1' as usize;
                self.toggle_work_view_section(index);
            }
            // Same comment bindings the dock detail uses, because it is the
            // same renderer: a digit toggles the comment it numbers and "a"
            // toggles every one.
            KeyCode::Char(digit @ '1'..='9') if key.modifiers.is_empty() => {
                if let Some(object_key) = self.work_view_comment_object_key() {
                    let index = digit as usize - '1' as usize;
                    self.toggle_work_view_comment(object_key, index);
                }
            }
            KeyCode::Char('a') if key.modifiers.is_empty() => {
                if let Some(object_key) = self.work_view_comment_object_key() {
                    self.toggle_all_work_view_comments(object_key);
                }
            }
            KeyCode::Char('x') if key.modifiers.is_empty() => self.fix_selected_pr_comment(),
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.next_work_index_refresh = std::time::Instant::now();
                if let Some(view) = self.state.work_view.as_ref() {
                    match view.projection {
                        crate::app::state::WorkProjection::PullRequests
                        | crate::app::state::WorkProjection::ReviewQueue => {
                            self.work_index_cache_bypass.github = true;
                        }
                        crate::app::state::WorkProjection::Tickets => {
                            self.work_index_cache_bypass.linear = true;
                        }
                        crate::app::state::WorkProjection::Missive => {
                            self.work_index_cache_bypass.missive = true;
                        }
                        crate::app::state::WorkProjection::Agents => {}
                    }
                }
                if let Some(state) = self.state.work_view.as_mut() {
                    state.refreshing = true;
                }
            }
            _ => {}
        }
        true
    }

    /// The pull requests the work view is showing, in render order.
    fn work_view_pr_items(&self) -> Vec<crate::ui::work_list_detail::PrItem<'_>> {
        let Some(view) = self.state.work_view.as_ref() else {
            return Vec::new();
        };
        let Some(snapshot) = view.snapshot.as_ref() else {
            return Vec::new();
        };
        crate::ui::work_list_detail::sorted_filtered_prs(
            &snapshot.items,
            &self.state.work_item_detail_cache,
            &view.search,
            view.sort,
            view.open_only,
            snapshot.observed_at,
            Some((
                &self.state.sidebar_work_filter,
                &self.state.work_index_session,
            )),
        )
    }

    /// The tickets the work view is showing, in render order.
    fn work_view_ticket_items(&self) -> Vec<crate::ui::work_list_detail::TicketItem<'_>> {
        let Some(view) = self.state.work_view.as_ref() else {
            return Vec::new();
        };
        let Some(snapshot) = view.snapshot.as_ref() else {
            return Vec::new();
        };
        crate::ui::work_list_detail::sorted_filtered_tickets(
            &snapshot.items,
            &self.state.work_item_detail_cache,
            &view.search,
            view.ticket_sort,
            view.ticket_open_only,
            snapshot.observed_at,
            crate::ui::dock::pr::focused_pr_key(&self.state).is_some(),
            Some((
                &self.state.sidebar_work_filter,
                &self.state.work_index_session,
            )),
        )
    }

    /// Toggle the section the alt digit names in the full work view.
    fn toggle_work_view_section(&mut self, index: usize) {
        let Some(object_key) = self.work_view_detail_object_key() else {
            return;
        };
        // A closed board card shows no detail, so a digit there would fold a
        // section the reader cannot see and only meets when they open the card.
        if !self.work_view_detail_is_visible() {
            return;
        }
        let Some(section) = self
            .work_view_detail_layout(&object_key)
            .and_then(|layout| layout.sections.get(index).copied())
            .map(|(_, section)| section)
        else {
            return;
        };
        if let Some(state) = self.state.work_view.as_mut() {
            state.object_view_mut(object_key).toggle_section(section);
        }
    }

    /// Fold the section whose header the full work view drew under this point.
    /// Returns whether a header was there, so the caller can fall through to the
    /// view's other click targets when it was not.
    fn fold_work_view_section_at(&mut self, column: u16, row: u16) -> bool {
        if !self.work_view_detail_is_visible() {
            return false;
        }
        let area = crate::ui::work_view::detail_inner_rect(self.state.view.terminal_area);
        if !self.state.point_in_rect(area, column, row) {
            return false;
        }
        let Some(object_key) = self.work_view_detail_object_key() else {
            return false;
        };
        let Some(layout) = self.work_view_detail_layout(&object_key) else {
            return false;
        };
        let view = match self.state.work_view.as_ref() {
            Some(state) => state.object_view(&object_key),
            None => return false,
        };
        let Some(section) = section_at_row(&layout, &view, area, row) else {
            return false;
        };
        if let Some(state) = self.state.work_view.as_mut() {
            state.object_view_mut(object_key).toggle_section(section);
        }
        true
    }

    /// The detail layout the work view is drawing for `object_key`.
    fn work_view_detail_layout(
        &self,
        object_key: &crate::app::state::WorkItemKey,
    ) -> Option<crate::ui::work_view::DetailLayout> {
        let state = self.state.work_view.as_ref()?;
        let view = state.object_view(object_key);
        // The render draws the detail inside the right-hand column, and its
        // width decides where every wrapped line falls, so the layout has to be
        // rebuilt against that rectangle rather than the whole screen.
        let area = crate::ui::work_view::detail_inner_rect(self.state.view.terminal_area);
        match state.projection {
            crate::app::state::WorkProjection::PullRequests => {
                let items = self.work_view_pr_items();
                let item = items
                    .into_iter()
                    .find(|item| same_work_object(&item.stable_key(), object_key))?;
                Some(crate::ui::work_view::pr_detail_layout(
                    &self.state,
                    &item,
                    &view,
                    &crate::ui::work_view::PrDetailControls {
                        checkout_menu: state.checkout_menu,
                        action_menu: state.pr_action_menu,
                        reviewer_picker: state.reviewer_picker.as_ref(),
                        pending_write: state.pending_write.as_ref(),
                        notice: None,
                    },
                    area,
                ))
            }
            crate::app::state::WorkProjection::Tickets => {
                let items = self.work_view_ticket_items();
                let item = items
                    .into_iter()
                    .find(|item| same_work_object(&item.stable_key(), object_key))?;
                Some(crate::ui::work_view::ticket_detail_layout(
                    &self.state,
                    &item,
                    &view,
                    &crate::ui::work_view::TicketDetailControls {
                        start_menu: state.ticket_start_menu,
                        transition_menu: state.ticket_transition_menu,
                        action_menu: state.ticket_more_menu,
                        comment_draft: state.ticket_comment_draft.as_deref(),
                        pending_write: state.pending_write.as_ref(),
                        notice: None,
                    },
                    area,
                ))
            }
            _ => None,
        }
    }

    /// The object whose comment list the full work view is showing, resolved
    /// the way the render resolves it.
    ///
    /// The render looks the selection up in the visible list and falls back to
    /// the first row when the selected item has been filtered out, which a
    /// refresh does routinely: a pull request that closes leaves the open-only
    /// list. Trusting `selected` directly would toggle a comment on an object
    /// nobody is looking at.
    fn work_view_detail_object_key(&self) -> Option<crate::app::state::WorkItemKey> {
        let keys = match self.state.work_view.as_ref()?.projection {
            crate::app::state::WorkProjection::PullRequests => self.visible_pr_view_keys(),
            crate::app::state::WorkProjection::Tickets => self.visible_ticket_view_keys(),
            _ => return None,
        };
        let selected = self.state.work_view.as_ref()?.selected.as_ref();
        let key = selected
            .and_then(|selected| keys.iter().find(|key| same_work_object(key, selected)))
            .or_else(|| keys.first())
            .cloned()?;
        Some(key)
    }

    /// Same object, but only when its comment list is on screen.
    fn work_view_comment_object_key(&self) -> Option<crate::app::state::WorkItemKey> {
        let key = self.work_view_detail_object_key()?;
        self.work_view_comments_are_visible(&key).then_some(key)
    }

    /// Whether the full work view is drawing this object's detail at all. A
    /// ticket board shows cards until one is opened, and the detail sub-tabs of
    /// a pull request are all details.
    fn work_view_detail_is_visible(&self) -> bool {
        self.state
            .work_view
            .as_ref()
            .is_some_and(|state| match state.projection {
                crate::app::state::WorkProjection::PullRequests => true,
                crate::app::state::WorkProjection::Tickets => {
                    state.ticket_layout != crate::app::state::LinearViewLayout::Board
                        || state.board_detail_open
                }
                _ => false,
            })
    }

    /// Whether the full work view is showing this object's comment list.
    /// Pull requests keep comments on the overview sub-tab only, and a ticket
    /// board shows none until a card is opened.
    fn work_view_comments_are_visible(&self, object_key: &crate::app::state::WorkItemKey) -> bool {
        let folded = self.state.work_view.as_ref().is_some_and(|state| {
            state
                .object_view(object_key)
                .section_is_collapsed(crate::app::state::DetailSection::Comments)
        });
        !folded
            && self.work_view_detail_is_visible()
            && self
                .state
                .work_view
                .as_ref()
                .is_some_and(|state| match state.projection {
                    crate::app::state::WorkProjection::PullRequests => {
                        state.object_view(object_key).tab
                            == crate::app::state::PrDetailTab::Overview
                    }
                    _ => true,
                })
    }

    fn toggle_work_view_comment(
        &mut self,
        object_key: crate::app::state::WorkItemKey,
        index: usize,
    ) {
        let Some(identity) = self
            .state
            .work_item_detail_cache
            .get(&object_key)
            .and_then(|detail| detail.comments.get(index))
            .map(crate::ui::work_list_detail::comment_identity)
        else {
            return;
        };
        if let Some(state) = self.state.work_view.as_mut() {
            state.object_view_mut(object_key).toggle_comment(identity);
        }
    }

    fn toggle_all_work_view_comments(&mut self, object_key: crate::app::state::WorkItemKey) {
        let identities = self
            .state
            .work_item_detail_cache
            .get(&object_key)
            .map(|detail| {
                detail
                    .comments
                    .iter()
                    .map(crate::ui::work_list_detail::comment_identity)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if identities.is_empty() {
            return;
        }
        if let Some(state) = self.state.work_view.as_mut() {
            state
                .object_view_mut(object_key)
                .toggle_all_comments(identities);
        }
    }

    fn selected_pr_detail(
        &self,
    ) -> Option<(
        crate::app::state::WorkItemKey,
        &crate::work_index::WorkItemDetail,
    )> {
        let keys = self.visible_pr_view_keys();
        let key = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected.clone())
            .or_else(|| keys.first().cloned())?;
        let detail = self.state.work_item_detail_cache.get(&key)?;
        Some((key, detail))
    }

    fn open_selected_pr_reviewer_picker(&mut self) {
        let keys = self.visible_pr_view_keys();
        let Some(key) = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected.clone())
            .or_else(|| keys.first().cloned())
        else {
            return;
        };
        let needs_fetch = self
            .state
            .work_item_detail_cache
            .get(&key)
            .is_none_or(|detail| {
                detail.collaborators.is_empty() && detail.collaborators_unavailable.is_none()
            });
        if needs_fetch {
            let result = crate::work_index::fetch_github_collaborators(
                &key.repo,
                &self.work_index_gh_program(),
                std::time::Instant::now() + crate::work_index::WORK_INDEX_TARGET_TIMEOUT,
            );
            let mut detail = self
                .state
                .work_item_detail_cache
                .get(&key)
                .cloned()
                .unwrap_or_else(crate::work_index::WorkItemDetail::empty);
            match result {
                Ok(collaborators) => {
                    detail.collaborators = collaborators;
                    detail.collaborators_unavailable = None;
                }
                Err(message) => {
                    detail.collaborators_unavailable = Some(message.clone());
                    if let Some(view) = self.state.work_view.as_mut() {
                        view.hint = Some(message);
                    }
                }
            }
            self.state
                .work_item_detail_cache
                .insert(key.clone(), detail);
        }
        if self
            .state
            .work_item_detail_cache
            .get(&key)
            .is_some_and(|detail| detail.collaborators_unavailable.is_none())
        {
            if let Some(view) = self.state.work_view.as_mut() {
                view.reviewer_picker = Some(Default::default());
            }
        }
    }

    fn add_selected_pr_reviewer(&mut self, login: String) {
        let Some((key, _)) = self.selected_pr_detail() else {
            return;
        };
        let Some(number) = key.pr_number else {
            return;
        };
        if let Some(view) = self.state.work_view.as_mut() {
            view.reviewer_picker = None;
            view.pending_write = Some(crate::work_index::WorkItemWrite::AddPullRequestReviewer {
                repo: key.repo,
                number,
                login,
            });
        }
        self.run_pending_work_view_write();
    }

    fn open_dock_pr_reviewer_picker(&mut self, key: crate::app::state::WorkItemKey) {
        let needs_fetch = self
            .state
            .work_item_detail_cache
            .get(&key)
            .is_none_or(|detail| {
                detail.collaborators.is_empty() && detail.collaborators_unavailable.is_none()
            });
        if needs_fetch {
            let result = crate::work_index::fetch_github_collaborators(
                &key.repo,
                &self.work_index_gh_program(),
                std::time::Instant::now() + crate::work_index::WORK_INDEX_TARGET_TIMEOUT,
            );
            let mut detail = self
                .state
                .work_item_detail_cache
                .get(&key)
                .cloned()
                .unwrap_or_else(crate::work_index::WorkItemDetail::empty);
            match result {
                Ok(collaborators) => {
                    detail.collaborators = collaborators;
                    detail.collaborators_unavailable = None;
                }
                Err(message) => {
                    detail.collaborators_unavailable = Some(message.clone());
                    self.state.dock_write_notice = Some(message);
                }
            }
            self.state
                .work_item_detail_cache
                .insert(key.clone(), detail);
        }
        if self
            .state
            .work_item_detail_cache
            .get(&key)
            .is_some_and(|detail| detail.collaborators_unavailable.is_none())
        {
            self.state
                .dock_object_views
                .entry(key)
                .or_default()
                .reviewer_picker = Some(Default::default());
        }
    }

    fn add_dock_pr_reviewer(&mut self, key: crate::app::state::WorkItemKey, login: String) {
        let Some(number) = key.pr_number else {
            return;
        };
        self.state
            .dock_object_views
            .entry(key.clone())
            .or_default()
            .reviewer_picker = None;
        self.state.dock_pending_write =
            Some(crate::work_index::WorkItemWrite::AddPullRequestReviewer {
                repo: key.repo,
                number,
                login,
            });
        self.run_pending_dock_write();
    }

    fn move_ticket_board_column(&mut self, delta: i64) {
        let columns = self
            .state
            .work_view
            .as_ref()
            .map(|view| crate::ui::work_view::ticket_board_columns(&self.state, view));
        let Some(columns) = columns else { return };
        let Some(view) = self.state.work_view.as_mut() else {
            return;
        };
        view.board_column = (view.board_column as i64 + delta).clamp(0, 4) as usize;
        let row = view.board_rows[view.board_column]
            .min(columns[view.board_column].len().saturating_sub(1));
        view.board_rows[view.board_column] = row;
        view.selected = columns[view.board_column].get(row).cloned();
        Self::reveal_ticket_board_row(view, self.state.view.terminal_area.height);
    }

    fn move_ticket_board_row(&mut self, delta: i64) {
        let columns = self
            .state
            .work_view
            .as_ref()
            .map(|view| crate::ui::work_view::ticket_board_columns(&self.state, view));
        let Some(columns) = columns else { return };
        let Some(view) = self.state.work_view.as_mut() else {
            return;
        };
        let column = view.board_column;
        if columns[column].is_empty() {
            view.selected = None;
            return;
        }
        let row = (view.board_rows[column] as i64 + delta)
            .clamp(0, columns[column].len().saturating_sub(1) as i64) as usize;
        view.board_rows[column] = row;
        view.selected = columns[column].get(row).cloned();
        Self::reveal_ticket_board_row(view, self.state.view.terminal_area.height);
    }

    fn reveal_ticket_board_row(view: &mut crate::app::state::WorkViewState, height: u16) {
        let column = view.board_column;
        let capacity = usize::from(height.saturating_sub(3)) / 4;
        let capacity = capacity.max(1);
        let row = view.board_rows[column];
        if row < view.board_scroll[column] {
            view.board_scroll[column] = row;
        } else if row >= view.board_scroll[column] + capacity {
            view.board_scroll[column] = row + 1 - capacity;
        }
    }

    pub(crate) fn visible_pr_view_keys(&self) -> Vec<crate::app::state::WorkItemKey> {
        self.work_view_pr_items()
            .iter()
            .map(crate::ui::work_list_detail::PrItem::stable_key)
            .collect()
    }

    fn move_pr_view_selection(&mut self, delta: i64) {
        if self
            .state
            .work_view
            .as_ref()
            .is_some_and(|view| view.projection == crate::app::state::WorkProjection::Missive)
        {
            self.move_missive_view_selection(delta);
            return;
        }
        let keys = if self
            .state
            .work_view
            .as_ref()
            .is_some_and(|view| view.projection == crate::app::state::WorkProjection::Tickets)
        {
            self.visible_ticket_view_keys()
        } else {
            self.visible_pr_view_keys()
        };
        if keys.is_empty() {
            return;
        }
        let current = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected.as_ref())
            .and_then(|selected| keys.iter().position(|key| key == selected))
            .unwrap_or(0);
        let next = (current as i64 + delta).clamp(0, keys.len().saturating_sub(1) as i64) as usize;
        if let Some(view) = self.state.work_view.as_mut() {
            view.selected = keys.get(next).cloned();
            view.hint = None;
        }
    }

    fn visible_missive_conversations(&self) -> Vec<crate::work_index::MissiveConversation> {
        let Some(view) = self.state.work_view.as_ref() else {
            return Vec::new();
        };
        let observed_at = view
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.observed_at)
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        view.snapshot
            .as_ref()
            .map(|snapshot| {
                crate::ui::work_list_detail::sorted_filtered_conversations(
                    &snapshot.conversations,
                    &view.search,
                    !view.open_only,
                    observed_at,
                )
                .into_iter()
                .map(|item| item.summary.clone())
                .collect()
            })
            .unwrap_or_default()
    }

    fn move_missive_view_selection(&mut self, delta: i64) {
        let conversations = self.visible_missive_conversations();
        if conversations.is_empty() {
            return;
        }
        let current = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected_missive.as_deref())
            .and_then(|selected| {
                conversations
                    .iter()
                    .position(|conversation| conversation.id == selected)
            })
            .unwrap_or(0);
        let next = (current as i64 + delta).clamp(0, conversations.len().saturating_sub(1) as i64)
            as usize;
        if let Some(view) = self.state.work_view.as_mut() {
            view.selected_missive = conversations.get(next).map(|item| item.id.clone());
            view.hint = None;
            view.missive_detail_scroll = 0;
        }
        self.next_work_index_refresh = std::time::Instant::now();
    }

    fn selected_missive_conversation(&self) -> Option<crate::work_index::MissiveConversation> {
        let conversations = self.visible_missive_conversations();
        let selected = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected_missive.as_deref());
        selected
            .and_then(|id| conversations.iter().find(|item| item.id == id))
            .or_else(|| conversations.first())
            .cloned()
    }

    fn copy_selected_missive_url(&mut self) {
        let Some(conversation) = self.selected_missive_conversation() else {
            return;
        };
        self.state.request_clipboard_write = Some(conversation.app_url.as_bytes().to_vec());
        if let Some(view) = self.state.work_view.as_mut() {
            view.hint = Some("Missive link copied".into());
        }
    }

    fn open_selected_missive_thread(&mut self) {
        let Some(conversation) = self.selected_missive_conversation() else {
            return;
        };
        let choice = self
            .state
            .work_view
            .as_ref()
            .and_then(|state| state.missive_start_menu)
            .unwrap_or_default();
        let directory = self
            .state
            .workspaces
            .iter()
            .find(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.panes.values())
                    .any(|pane| {
                        self.state
                            .terminals
                            .get(&pane.attached_terminal_id)
                            .is_some_and(|terminal| {
                                terminal
                                    .effective_work_context()
                                    .missive_urls
                                    .iter()
                                    .any(|url| url == &conversation.app_url)
                            })
                    })
            })
            .map(|workspace| workspace.identity_cwd.clone())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
            });
        let mut home = self.state.new_home_state();
        home.directory = directory.clone();
        home.ref_directory = directory;
        home.workspace = match choice {
            crate::app::state::PrCheckoutChoice::CurrentCheckout => {
                crate::app::home::HomeWorkspace::CurrentCheckout
            }
            crate::app::state::PrCheckoutChoice::NewWorktree => {
                crate::app::home::HomeWorkspace::NewWorktree
            }
        };
        home.prompt = format!("{}\n\n{}", conversation.subject, conversation.web_url);
        home.missive = Some(crate::app::home::HomeMissiveContext {
            app_url: conversation.app_url,
            web_url: conversation.web_url,
            subject: conversation.subject,
        });
        self.state.work_view = None;
        self.state.inbox = None;
        self.state.home = Some(home);
    }

    pub(crate) fn visible_ticket_view_keys(&self) -> Vec<crate::app::state::WorkItemKey> {
        self.work_view_ticket_items()
            .iter()
            .map(crate::ui::work_list_detail::TicketItem::stable_key)
            .collect()
    }

    fn selected_ticket_parts(
        &self,
    ) -> Option<(
        crate::app::state::WorkItemKey,
        crate::work_index::WorkTicket,
        String,
    )> {
        let keys = self.visible_ticket_view_keys();
        let key = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected.clone())
            .filter(|key| key.ticket_id.is_some())
            .or_else(|| keys.first().cloned())?;
        let ticket_id = key.ticket_id.as_deref()?;
        let source = self
            .state
            .work_view
            .as_ref()?
            .snapshot
            .as_ref()?
            .items
            .iter()
            .find(|item| {
                item.ticket_details
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(ticket_id))
            })?;
        let ticket = source
            .ticket_details
            .iter()
            .find(|ticket| ticket.identifier.eq_ignore_ascii_case(ticket_id))?
            .clone();
        Some((key, ticket, source.repo.clone()))
    }

    fn open_selected_ticket_thread(&mut self) {
        let Some((_key, ticket, repo)) = self.selected_ticket_parts() else {
            return;
        };
        let choice = self
            .state
            .work_view
            .as_ref()
            .and_then(|state| state.ticket_start_menu)
            .unwrap_or_default();
        let prompt = Self::ticket_work_prompt(&ticket);
        self.open_ticket_home(ticket, repo, prompt, choice);
    }

    fn ticket_work_prompt(ticket: &crate::work_index::WorkTicket) -> String {
        match (ticket.title.as_deref(), ticket.description.as_deref()) {
            (Some(title), Some(description)) if !description.trim().is_empty() => {
                format!("{title}\n\n{description}")
            }
            (Some(title), _) => title.to_string(),
            (_, Some(description)) => description.to_string(),
            _ => ticket.identifier.clone(),
        }
    }

    fn open_ticket_home(
        &mut self,
        ticket: crate::work_index::WorkTicket,
        repo: String,
        prompt: String,
        choice: crate::app::state::PrCheckoutChoice,
    ) {
        let directory = self.ticket_checkout_directory(&ticket.identifier, &repo);
        let mut home = self.state.new_home_state();
        home.directory = directory.clone();
        home.ref_directory = directory.clone();
        home.ref_repo_root = self
            .state
            .git_root_for_cwd
            .get(&directory)
            .and_then(Clone::clone)
            .or_else(|| crate::app::worktrees::worktree_repo_root(&directory));
        home.workspace = match choice {
            crate::app::state::PrCheckoutChoice::CurrentCheckout => {
                crate::app::home::HomeWorkspace::CurrentCheckout
            }
            crate::app::state::PrCheckoutChoice::NewWorktree => {
                crate::app::home::HomeWorkspace::NewWorktree
            }
        };
        home.ticket = Some(crate::app::home::HomeTicketContext {
            identifier: ticket.identifier.clone(),
            title: ticket
                .title
                .clone()
                .unwrap_or_else(|| "(untitled ticket)".into()),
            url: ticket.url.clone().unwrap_or_else(|| {
                crate::work_context::linear_ticket_url(&ticket.identifier).unwrap_or_default()
            }),
        });
        home.prompt = prompt;
        self.state.work_view = None;
        self.state.inbox = None;
        self.state.home = Some(home);
    }

    fn ticket_checkout_directory(&self, identifier: &str, repo: &str) -> std::path::PathBuf {
        self.state
            .workspaces
            .iter()
            .find(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.panes.values())
                    .any(|pane| {
                        self.state
                            .terminals
                            .get(&pane.attached_terminal_id)
                            .is_some_and(|terminal| {
                                let context = terminal.effective_work_context();
                                context
                                    .ticket_ids
                                    .iter()
                                    .any(|ticket| ticket.eq_ignore_ascii_case(identifier))
                                    || (!repo.is_empty()
                                        && context.repo.as_deref().is_some_and(|candidate| {
                                            crate::work_context::repo_slugs_match(candidate, repo)
                                        }))
                            })
                    })
            })
            .map(|workspace| workspace.identity_cwd.clone())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
            })
    }

    fn stage_ticket_transition(&mut self, choice: crate::app::state::TicketTransitionChoice) {
        let Some((_key, ticket, _repo)) = self.selected_ticket_parts() else {
            return;
        };
        if ticket
            .state
            .as_deref()
            .is_some_and(|state| state.eq_ignore_ascii_case(choice.label()))
        {
            return;
        }
        if let Some(state) = self.state.work_view.as_mut() {
            state.ticket_transition_menu = None;
            state.pending_write = Some(crate::work_index::WorkItemWrite::TransitionTicket {
                identifier: ticket.identifier,
                state: choice.label().to_string(),
            });
        }
    }

    fn stage_ticket_link_pr(&mut self) {
        let Some((_key, ticket, _repo)) = self.selected_ticket_parts() else {
            return;
        };
        let Some(pr) = crate::ui::dock::pr::focused_pr_key(&self.state) else {
            return;
        };
        let (Some(number), Some(url)) = (pr.pr_number, pr.pr_url) else {
            return;
        };
        if let Some(state) = self.state.work_view.as_mut() {
            state.pending_write = Some(crate::work_index::WorkItemWrite::LinkTicketPullRequest {
                identifier: ticket.identifier,
                title: format!("{}#{number}", pr.repo),
                url,
            });
        }
    }

    fn selected_ticket_action_context(
        &self,
    ) -> Option<crate::ui::ticket_actions::TicketActionContext> {
        let (key, ticket, _repo) = self.selected_ticket_parts()?;
        Some(crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &ticket,
            self.state.work_item_detail_cache.get(&key),
            self.state.work_index_session.linear.viewer.as_deref(),
            self.state.work_index_session.linear_viewer_identity(),
            crate::ui::dock::pr::focused_pr_key(&self.state).is_some(),
        ))
    }

    fn activate_ticket_action(&mut self, action: crate::ui::ticket_actions::TicketAction) {
        use crate::ui::ticket_actions::{TicketAction, TicketActionMenuPage};
        let Some((key, ticket, repo)) = self.selected_ticket_parts() else {
            return;
        };
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &ticket,
            self.state.work_item_detail_cache.get(&key),
            self.state.work_index_session.linear.viewer.as_deref(),
            self.state.work_index_session.linear_viewer_identity(),
            crate::ui::dock::pr::focused_pr_key(&self.state).is_some(),
        );
        match action {
            TicketAction::TransitionMenu | TicketAction::PriorityMenu => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu =
                        Some(crate::ui::ticket_actions::TicketActionMenuState {
                            page: if action == TicketAction::TransitionMenu {
                                TicketActionMenuPage::Transitions
                            } else {
                                TicketActionMenuPage::Priorities
                            },
                            selected: 0,
                        });
                }
            }
            TicketAction::Refresh => {
                self.next_work_index_refresh = std::time::Instant::now();
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                    state.refreshing = true;
                }
            }
            TicketAction::AskQuestion => {
                self.open_ticket_home(
                    ticket,
                    repo,
                    format!("About {} ({}): ", context.identifier, context.title),
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::Explain => {
                self.open_ticket_home(
                    ticket,
                    repo,
                    format!(
                        "Summarise {} ({}). Include the ticket, linked pull requests, and the acceptance criteria block.",
                        context.identifier, context.title
                    ),
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::WorkInThread => self.open_ticket_home(
                ticket.clone(),
                repo,
                Self::ticket_work_prompt(&ticket),
                crate::app::state::PrCheckoutChoice::CurrentCheckout,
            ),
            TicketAction::Transition(choice) => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                    state.pending_write =
                        Some(crate::work_index::WorkItemWrite::TransitionTicket {
                            identifier: ticket.identifier,
                            state: choice.label().to_string(),
                        });
                }
            }
            TicketAction::AssignToMe => {
                let Some(viewer) = context.viewer_id else {
                    return;
                };
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                    state.pending_write = Some(crate::work_index::WorkItemWrite::AssignTicket {
                        identifier: ticket.identifier,
                        assignee: viewer,
                    });
                }
            }
            TicketAction::SetPriority(priority) => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                    state.pending_write =
                        Some(crate::work_index::WorkItemWrite::SetTicketPriority {
                            identifier: ticket.identifier,
                            priority,
                        });
                }
            }
            TicketAction::LinkPr => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                }
                self.stage_ticket_link_pr();
            }
            TicketAction::Comment => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                    state.ticket_comment_draft = Some(String::new());
                }
            }
            TicketAction::OpenInLinear => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                }
                let url = self
                    .selected_ticket_action_context()
                    .and_then(|context| context.url);
                if let Some(url) = url {
                    if let Err(error) = crate::platform::open_url(&url) {
                        if let Some(state) = self.state.work_view.as_mut() {
                            state.hint = Some(format!("could not open ticket: {error}"));
                        }
                    }
                }
            }
            TicketAction::CopyLink => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                }
                if let Some(url) = context.url {
                    self.state.request_clipboard_write = Some(url.into_bytes());
                }
            }
            TicketAction::CopyIdentifier => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                }
                self.state.request_clipboard_write = Some(ticket.identifier.into_bytes());
            }
            TicketAction::Cancel => {
                if let Some(state) = self.state.work_view.as_mut() {
                    state.ticket_more_menu = None;
                    state.pending_write =
                        Some(crate::work_index::WorkItemWrite::TransitionTicket {
                            identifier: ticket.identifier,
                            state: "Canceled".into(),
                        });
                }
            }
        }
    }

    fn stage_ticket_comment(&mut self) {
        let Some((_key, ticket, _repo)) = self.selected_ticket_parts() else {
            return;
        };
        let Some(body) = self
            .state
            .work_view
            .as_ref()
            .and_then(|state| state.ticket_comment_draft.as_deref())
            .map(str::trim)
            .filter(|body| !body.is_empty())
            .map(str::to_string)
        else {
            return;
        };
        if let Some(state) = self.state.work_view.as_mut() {
            state.ticket_comment_draft = None;
            state.pending_write = Some(crate::work_index::WorkItemWrite::CommentOnTicket {
                identifier: ticket.identifier,
                body,
            });
        }
    }

    fn run_pending_work_view_write(&mut self) {
        let Some(write) = self
            .state
            .work_view
            .as_mut()
            .and_then(|state| state.pending_write.take())
        else {
            return;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let result = crate::work_index::run_work_item_write(
            &write,
            &self.work_index_gh_program(),
            &self.work_index_linearis_program(),
            deadline,
        );
        if let Some(key) = write.target() {
            self.state.work_item_detail_cache.remove(&key);
        }
        let succeeded = result.is_ok();
        let message = match result {
            Ok(message) | Err(message) => message,
        };
        if let Some(state) = self.state.work_view.as_mut() {
            state.hint = Some(message);
            state.refreshing = succeeded;
        }
        if succeeded {
            self.next_work_index_refresh = std::time::Instant::now();
            match write {
                crate::work_index::WorkItemWrite::CommentOnPullRequest { .. }
                | crate::work_index::WorkItemWrite::ApprovePullRequest { .. }
                | crate::work_index::WorkItemWrite::ClosePullRequest { .. }
                | crate::work_index::WorkItemWrite::MarkPullRequestDraft { .. }
                | crate::work_index::WorkItemWrite::MarkPullRequestReady { .. }
                | crate::work_index::WorkItemWrite::MergePullRequest { .. }
                | crate::work_index::WorkItemWrite::AddPullRequestReviewer { .. } => {
                    self.work_index_cache_bypass.github = true;
                }
                _ => self.work_index_cache_bypass.linear = true,
            }
        }
    }

    fn selected_pr_parts(
        &self,
    ) -> Option<(
        crate::app::state::WorkItemKey,
        String,
        crate::app::home::HomePrContext,
    )> {
        let keys = self.visible_pr_view_keys();
        let key = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected.clone())
            .or_else(|| keys.first().cloned())?;
        let summary = self
            .state
            .work_view
            .as_ref()?
            .snapshot
            .as_ref()?
            .items
            .iter()
            .find(|item| item.repo == key.repo && item.pr_number == key.pr_number)?;
        let number = summary.pr_number?;
        let url = summary.pr_url.clone().or_else(|| {
            self.state
                .work_item_detail_cache
                .get(&key)
                .and_then(|detail| detail.url.clone())
        })?;
        let head = self
            .state
            .work_item_detail_cache
            .get(&key)
            .and_then(|detail| detail.head_ref_name.clone())
            .or_else(|| summary.branch.clone())?;
        let pr = crate::app::home::HomePrContext {
            url,
            number,
            repo: summary.repo.clone(),
        };
        Some((key, head, pr))
    }

    fn open_selected_pr_checkout(&mut self) {
        let Some((_key, head, pr)) = self.selected_pr_parts() else {
            return;
        };
        let choice = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.checkout_menu)
            .unwrap_or_default();
        self.open_pr_home(head, choice, String::new(), pr);
    }

    fn fix_selected_pr_comment(&mut self) {
        let Some((key, head, pr)) = self.selected_pr_parts() else {
            return;
        };
        let Some(body) = self
            .state
            .work_item_detail_cache
            .get(&key)
            .and_then(|detail| {
                detail
                    .comments
                    .iter()
                    .max_by_key(|comment| comment.created_at)
            })
            .map(|comment| comment.body.clone())
        else {
            return;
        };
        self.open_pr_home(
            head,
            crate::app::state::PrCheckoutChoice::CurrentCheckout,
            body,
            pr,
        );
    }

    fn open_pr_home(
        &mut self,
        head: String,
        choice: crate::app::state::PrCheckoutChoice,
        prompt: String,
        pr: crate::app::home::HomePrContext,
    ) {
        let directory = self
            .state
            .workspaces
            .iter()
            .find(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.panes.values())
                    .any(|pane| {
                        self.state
                            .terminals
                            .get(&pane.attached_terminal_id)
                            .and_then(|terminal| terminal.effective_work_context().repo.as_deref())
                            .is_some_and(|repo| {
                                crate::work_context::repo_slugs_match(repo, &pr.repo)
                            })
                    })
            })
            .map(|workspace| workspace.identity_cwd.clone())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
            });
        let directory = self
            .state
            .git_root_for_cwd
            .get(&directory)
            .and_then(Clone::clone)
            .unwrap_or(directory);
        let mut home = self.state.new_home_state();
        home.directory = directory.clone();
        home.ref_directory = directory.clone();
        home.ref_repo_root = Some(directory);
        home.workspace = match choice {
            crate::app::state::PrCheckoutChoice::CurrentCheckout => {
                crate::app::home::HomeWorkspace::CurrentCheckout
            }
            crate::app::state::PrCheckoutChoice::NewWorktree => {
                crate::app::home::HomeWorkspace::NewWorktree
            }
        };
        home.selected_ref = Some(crate::app::home_refs::HomeRef {
            name: head,
            oid: String::new(),
            tag: None,
        });
        home.pr = Some(pr);
        home.prompt = prompt;
        self.state.work_view = None;
        self.state.inbox = None;
        self.state.home = Some(home);
    }

    fn selected_pr_action_table(&self) -> Vec<crate::ui::work_list_detail::PrAction> {
        self.state
            .work_view
            .as_ref()
            .and_then(|view| view.selected.clone())
            .or_else(|| self.visible_pr_view_keys().first().cloned())
            .and_then(|key| self.pr_action_table(&key))
            .unwrap_or_default()
    }

    pub(crate) fn pr_action_table(
        &self,
        key: &crate::app::state::WorkItemKey,
    ) -> Option<Vec<crate::ui::work_list_detail::PrAction>> {
        let summary = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.snapshot.as_ref())
            .or(self.state.work_index_snapshot.as_ref())?
            .items
            .iter()
            .find(|item| item.repo == key.repo && item.pr_number == key.pr_number)?;
        let checkout_available = self.pr_head_ref(key).is_some();
        Some(
            crate::ui::work_list_detail::PrItem {
                summary,
                cached_detail: self.state.work_item_detail_cache.get(key),
                observed_at: std::time::SystemTime::now(),
            }
            .action_table(self.state.pr_merge_method, checkout_available),
        )
    }

    fn activate_selected_pr_action(&mut self, kind: crate::ui::work_list_detail::PrActionKind) {
        let Some((key, _, _)) = self.selected_pr_parts() else {
            return;
        };
        self.activate_pr_action(key, kind);
    }

    pub(crate) fn activate_pr_action(
        &mut self,
        key: crate::app::state::WorkItemKey,
        kind: crate::ui::work_list_detail::PrActionKind,
    ) {
        use crate::ui::work_list_detail::PrActionKind;
        let enabled = self
            .pr_action_table(&key)
            .into_iter()
            .flatten()
            .any(|action| action.kind == kind && action.enabled());
        if !enabled {
            return;
        }
        match kind {
            PrActionKind::Refresh => {
                self.state.work_item_detail_cache.remove(&key);
                self.next_work_index_refresh = std::time::Instant::now();
                if let Some(view) = self.state.work_view.as_mut() {
                    view.refreshing = true;
                }
            }
            PrActionKind::AskQuestion | PrActionKind::Explain | PrActionKind::FixFindings => {
                let findings_prompt = if kind == PrActionKind::FixFindings {
                    let Some(number) = key.pr_number else {
                        return;
                    };
                    let Some(threads) = crate::work_index::fetch_unresolved_review_threads(
                        &key.repo,
                        number,
                        &self.work_index_gh_program(),
                        std::time::Instant::now() + crate::work_index::WORK_INDEX_TARGET_TIMEOUT,
                    ) else {
                        return;
                    };
                    let mut detail = self
                        .state
                        .work_item_detail_cache
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(crate::work_index::WorkItemDetail::empty);
                    detail.unresolved_review_threads = Some(threads.count);
                    self.state
                        .work_item_detail_cache
                        .insert(key.clone(), detail);
                    if threads.count == 0 {
                        return;
                    }
                    Some(
                        threads
                            .comments
                            .iter()
                            .map(|comment| {
                                let line = comment
                                    .line
                                    .map_or_else(|| "?".into(), |line| line.to_string());
                                format!(
                                    "{}:{} — {}: {}",
                                    comment.path, line, comment.author, comment.body
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                    )
                } else {
                    None
                };
                let parts = self.pr_parts_for_key(&key);
                let Some((head, pr, title)) = parts else {
                    return;
                };
                let prompt = match kind {
                    PrActionKind::AskQuestion => {
                        format!("About PR #{} ({}): ", pr.number, title)
                    }
                    PrActionKind::Explain => {
                        "Walk the diff for this PR and explain what changed, why, and what I should read closely."
                            .into()
                    }
                    PrActionKind::FixFindings => findings_prompt.unwrap_or_default(),
                    _ => String::new(),
                };
                self.open_pr_home(
                    head,
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                    prompt,
                    pr,
                );
                if kind == PrActionKind::Explain {
                    if let Some(home) = self.state.home.as_mut() {
                        home.selected_ref = None;
                    }
                    self.dispatch_home_prompt();
                }
            }
            PrActionKind::OpenOnGithub => {
                if let Some(number) = key.pr_number {
                    self.state.request_pr_command = Some(crate::app::state::PrCommandRequest {
                        repo: key.repo,
                        number,
                        action: crate::app::state::PrCommandAction::OpenOnGithub,
                    });
                }
            }
            PrActionKind::CopyLink => {
                if let Some(url) = self.pr_url_for_key(&key) {
                    self.state.request_clipboard_write = Some(url.into_bytes());
                }
            }
            PrActionKind::CheckOut => {}
            PrActionKind::ConvertToDraft
            | PrActionKind::MarkReady
            | PrActionKind::EnableAutoMerge(_)
            | PrActionKind::DisableAutoMerge
            | PrActionKind::Merge(_)
            | PrActionKind::Close => {
                self.state.pr_action_confirmation =
                    Some(crate::app::state::PrActionConfirmation { key, action: kind });
            }
        }
    }

    fn pr_parts_for_key(
        &self,
        key: &crate::app::state::WorkItemKey,
    ) -> Option<(String, crate::app::home::HomePrContext, String)> {
        let summary = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.snapshot.as_ref())
            .or(self.state.work_index_snapshot.as_ref())?
            .items
            .iter()
            .find(|item| item.repo == key.repo && item.pr_number == key.pr_number)?;
        let number = summary.pr_number?;
        let url = self.pr_url_for_key(key)?;
        let head = self.pr_head_ref(key)?;
        Some((
            head,
            crate::app::home::HomePrContext {
                url,
                number,
                repo: summary.repo.clone(),
            },
            summary.pr_title.clone().unwrap_or_default(),
        ))
    }

    fn pr_url_for_key(&self, key: &crate::app::state::WorkItemKey) -> Option<String> {
        key.pr_url.clone().or_else(|| {
            self.state
                .work_item_detail_cache
                .get(key)
                .and_then(|detail| detail.url.clone())
        })
    }

    pub(super) fn handle_pr_action_confirmation_key(&mut self, key: KeyEvent) -> bool {
        let Some(confirmation) = self.state.pr_action_confirmation.clone() else {
            return false;
        };
        match key.code {
            KeyCode::Enter | KeyCode::Char('y' | 'Y') if key.modifiers.is_empty() => {
                self.state.pr_action_confirmation = None;
                self.execute_pr_action_confirmation(confirmation);
            }
            KeyCode::Esc | KeyCode::Char('n' | 'N') if key.modifiers.is_empty() => {
                self.state.pr_action_confirmation = None;
            }
            _ => {}
        }
        true
    }

    fn execute_pr_action_confirmation(
        &mut self,
        confirmation: crate::app::state::PrActionConfirmation,
    ) {
        use crate::ui::work_list_detail::PrActionKind;
        let Some(number) = confirmation.key.pr_number else {
            return;
        };
        let repo = confirmation.key.repo;
        match confirmation.action {
            PrActionKind::Merge(method) => {
                self.state.request_pr_command = Some(crate::app::state::PrCommandRequest {
                    repo,
                    number,
                    action: crate::app::state::PrCommandAction::Merge(method),
                });
            }
            action => {
                let write = match action {
                    PrActionKind::ConvertToDraft => {
                        crate::work_index::WorkItemWrite::MarkPullRequestDraft { repo, number }
                    }
                    PrActionKind::MarkReady => {
                        crate::work_index::WorkItemWrite::MarkPullRequestReady { repo, number }
                    }
                    PrActionKind::EnableAutoMerge(method) => {
                        crate::work_index::WorkItemWrite::SetPullRequestAutoMerge {
                            repo,
                            number,
                            enabled: true,
                            method,
                        }
                    }
                    PrActionKind::DisableAutoMerge => {
                        crate::work_index::WorkItemWrite::SetPullRequestAutoMerge {
                            repo,
                            number,
                            enabled: false,
                            method: self.state.pr_merge_method,
                        }
                    }
                    PrActionKind::Close => {
                        crate::work_index::WorkItemWrite::ClosePullRequest { repo, number }
                    }
                    _ => return,
                };
                self.state.dock_pending_write = Some(write);
                self.run_pending_dock_write();
                self.next_work_index_refresh = std::time::Instant::now();
            }
        }
    }

    /// Branch to check out for `key`: the fetched head, else the branch the
    /// work index recorded for the pull request.
    fn pr_head_ref(&self, key: &crate::app::state::WorkItemKey) -> Option<String> {
        self.state
            .work_item_detail_cache
            .get(key)
            .and_then(|detail| detail.head_ref_name.clone())
            .or_else(|| {
                self.state
                    .work_index_snapshot
                    .as_ref()?
                    .items
                    .iter()
                    .find(|item| item.repo == key.repo && item.pr_number == key.pr_number)
                    .and_then(|item| item.branch.clone())
            })
    }

    /// Whether the reader folded this object's comment section away in the
    /// dock. The comment digits address the list by position, so with the list
    /// off screen they have no subject and belong to the pane again.
    fn dock_comments_are_folded(&self, object_key: &crate::app::state::WorkItemKey) -> bool {
        self.state
            .dock_object_views
            .get(object_key)
            .is_some_and(|view| {
                view.section_is_collapsed(crate::app::state::DetailSection::Comments)
            })
    }

    /// Whether a confirmation, menu or picker on the dock pull-request surface
    /// holds the keyboard, so the detail's own bindings have no turn.
    fn dock_pr_modal_owns_keyboard(&self) -> bool {
        self.state.dock_pending_write.is_some()
            || self.state.dock_pr_checkout_menu.is_some()
            || self.state.dock_pr_action_menu.is_some()
            || crate::ui::dock::pr::focused_pr_key(&self.state).is_some_and(|key| {
                self.state
                    .dock_object_views
                    .get(&key)
                    .is_some_and(|view| view.reviewer_picker.is_some())
            })
    }

    /// The same question for the dock Linear surface.
    fn dock_linear_modal_owns_keyboard(&self) -> bool {
        self.state.dock_pending_write.is_some()
            || self.state.dock_ticket_start_menu.is_some()
            || self.state.dock_ticket_action_menu.is_some()
            || self.state.dock_ticket_comment_draft.is_some()
    }

    /// Toggle the section the alt digit names in a dock detail. The order comes
    /// from the render itself, so a view that hides a section never leaves a
    /// gap in the numbering.
    fn toggle_dock_detail_section(
        &mut self,
        object_key: crate::app::state::WorkItemKey,
        layout: Option<crate::ui::work_view::DetailLayout>,
        index: usize,
    ) -> bool {
        let Some((_, section)) = layout.and_then(|layout| layout.sections.get(index).copied())
        else {
            return false;
        };
        self.state
            .dock_object_views
            .entry(object_key)
            .or_default()
            .toggle_section(section);
        true
    }

    /// Expand or collapse one comment in an object detail view. `index` is
    /// zero-based; the render offers it as digit `index + 1`.
    fn toggle_dock_detail_comment(
        &mut self,
        object_key: crate::app::state::WorkItemKey,
        index: usize,
    ) -> bool {
        if self.dock_comments_are_folded(&object_key) {
            return false;
        }
        let Some(identity) = self
            .state
            .work_item_detail_cache
            .get(&object_key)
            .and_then(|detail| detail.comments.get(index))
            .map(crate::ui::work_list_detail::comment_identity)
        else {
            return false;
        };
        self.state
            .dock_object_views
            .entry(object_key)
            .or_default()
            .toggle_comment(identity);
        true
    }

    /// Expand every comment, or collapse them all when they already are. This
    /// is the only way to reach a comment past the ninth, which has no digit.
    fn toggle_all_dock_detail_comments(
        &mut self,
        object_key: crate::app::state::WorkItemKey,
    ) -> bool {
        if self.dock_comments_are_folded(&object_key) {
            return false;
        }
        let identities = self
            .state
            .work_item_detail_cache
            .get(&object_key)
            .map(|detail| {
                detail
                    .comments
                    .iter()
                    .map(crate::ui::work_list_detail::comment_identity)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if identities.is_empty() {
            return false;
        }
        self.state
            .dock_object_views
            .entry(object_key)
            .or_default()
            .toggle_all_comments(identities);
        true
    }

    fn handle_dock_pr_key(&mut self, key: &TerminalKey) -> bool {
        let previewed = self.state.dock_collapsed
            && self
                .state
                .dock_object_preview
                .as_ref()
                .is_some_and(|object| object.surface == crate::app::DockSurface::Pr);
        let dock_hosted =
            !self.state.dock_collapsed && self.state.dock_tab == Some(crate::app::DockSurface::Pr);
        if !(previewed || dock_hosted) || !self.state.dock_pr_focused {
            return false;
        }
        let event = key.as_key_event();
        // Alt digits fold sections; they are the only modified keys the detail
        // claims besides the shifted plus.
        let alt_digit = (event.modifiers == crossterm::event::KeyModifiers::ALT)
            .then(|| match event.code {
                KeyCode::Char(digit @ '1'..='9') => Some(digit as usize - '1' as usize),
                _ => None,
            })
            .flatten();
        if let Some(index) = alt_digit {
            // A confirmation or an open menu owns the keyboard. Folding behind
            // it would change a detail the reader cannot see, and letting the
            // key through would reach the pane while a prompt is up, so it is
            // swallowed instead.
            if self.dock_pr_modal_owns_keyboard() {
                return true;
            }
            let area = dock_detail_area(&self.state);
            let layout = crate::ui::dock::pr::focused_pr_layout(&self.state, area);
            return crate::ui::dock::pr::focused_pr_key(&self.state).is_some_and(|object_key| {
                self.toggle_dock_detail_section(object_key, layout, index)
            });
        }
        if !(event.modifiers.is_empty()
            || event.code == KeyCode::Char('+')
                && event.modifiers == crossterm::event::KeyModifiers::SHIFT)
        {
            return false;
        }
        if self.handle_pending_dock_write_key(event) {
            return true;
        }
        let focused_key = crate::ui::dock::pr::focused_pr_key(&self.state);
        if let Some((object_key, picker)) = focused_key.as_ref().and_then(|object_key| {
            self.state
                .dock_object_views
                .get(object_key)
                .and_then(|view| view.reviewer_picker.clone())
                .map(|picker| (object_key.clone(), picker))
        }) {
            let collaborators = self
                .state
                .work_item_detail_cache
                .get(&object_key)
                .map(|detail| detail.collaborators.clone())
                .unwrap_or_default();
            let matches = picker.filter.matches(&collaborators);
            match event.code {
                KeyCode::Esc => {
                    self.state
                        .dock_object_views
                        .entry(object_key)
                        .or_default()
                        .reviewer_picker = None;
                }
                KeyCode::Backspace => {
                    if let Some(picker) = self
                        .state
                        .dock_object_views
                        .entry(object_key)
                        .or_default()
                        .reviewer_picker
                        .as_mut()
                    {
                        picker.filter.pop();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(picker) = self
                        .state
                        .dock_object_views
                        .entry(object_key)
                        .or_default()
                        .reviewer_picker
                        .as_mut()
                    {
                        picker.filter.move_selection(-1, matches.len());
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(picker) = self
                        .state
                        .dock_object_views
                        .entry(object_key)
                        .or_default()
                        .reviewer_picker
                        .as_mut()
                    {
                        picker.filter.move_selection(1, matches.len());
                    }
                }
                KeyCode::Enter => {
                    if let Some((_, login)) = matches.get(picker.filter.selected) {
                        self.add_dock_pr_reviewer(object_key, (*login).to_string());
                    }
                }
                KeyCode::Char(character) => {
                    if let Some(picker) = self
                        .state
                        .dock_object_views
                        .entry(object_key)
                        .or_default()
                        .reviewer_picker
                        .as_mut()
                    {
                        picker.filter.push(character);
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some((object_key, selected)) = focused_key.as_ref().and_then(|object_key| {
            self.state
                .dock_object_views
                .get(object_key)
                .and_then(|view| view.tab_picker)
                .map(|selected| (object_key.clone(), selected))
        }) {
            let count = crate::app::state::PrDetailTab::ALL.len();
            let view = self.state.dock_object_views.entry(object_key).or_default();
            match event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    view.tab_picker = Some(selected.saturating_sub(1));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    view.tab_picker = Some((selected + 1).min(count.saturating_sub(1)));
                }
                KeyCode::Enter => {
                    view.tab = crate::app::state::PrDetailTab::ALL[selected];
                    view.scroll = 0;
                    view.tab_picker = None;
                }
                KeyCode::Esc => view.tab_picker = None,
                _ => {}
            }
            return true;
        }
        if let Some(menu) = self.state.dock_pr_action_menu {
            let actions = crate::ui::dock::pr::focused_pr_key(&self.state)
                .and_then(|key| self.pr_action_table(&key))
                .unwrap_or_default();
            match event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(menu) = self.state.dock_pr_action_menu.as_mut() {
                        crate::ui::pr_actions::move_selection(menu, &actions, -1);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(menu) = self.state.dock_pr_action_menu.as_mut() {
                        crate::ui::pr_actions::move_selection(menu, &actions, 1);
                    }
                }
                KeyCode::Enter => {
                    if let Some(action) = crate::ui::pr_actions::menu_actions(&actions)
                        .get(menu.selected)
                        .filter(|action| action.enabled())
                    {
                        let kind = action.kind;
                        self.state.dock_pr_action_menu = None;
                        if let Some(key) = crate::ui::dock::pr::focused_pr_key(&self.state) {
                            self.activate_pr_action(key, kind);
                        }
                    }
                }
                KeyCode::Esc => self.state.dock_pr_action_menu = None,
                _ => {}
            }
            return true;
        }
        if let Some(choice) = self.state.dock_pr_checkout_menu {
            match event.code {
                KeyCode::Up | KeyCode::Down => {
                    self.state.dock_pr_checkout_menu = Some(match choice {
                        crate::app::state::PrCheckoutChoice::CurrentCheckout => {
                            crate::app::state::PrCheckoutChoice::NewWorktree
                        }
                        crate::app::state::PrCheckoutChoice::NewWorktree => {
                            crate::app::state::PrCheckoutChoice::CurrentCheckout
                        }
                    });
                }
                KeyCode::Enter => self.open_dock_pr_checkout(choice),
                KeyCode::Esc => self.state.dock_pr_checkout_menu = None,
                _ => {}
            }
            return true;
        }
        match event.code {
            KeyCode::Tab => {
                if let Some(object_key) = focused_key {
                    let host_width = if previewed {
                        self.state.view.terminal_area.width
                    } else {
                        self.state.view.dock_body_rect.width
                    };
                    let narrow = host_width < 60;
                    let view = self.state.dock_object_views.entry(object_key).or_default();
                    if narrow {
                        view.scroll = 0;
                        view.tab_picker = crate::app::state::PrDetailTab::ALL
                            .iter()
                            .position(|tab| *tab == view.tab);
                    } else {
                        view.tab = view.tab.next();
                        view.scroll = 0;
                    }
                }
            }
            KeyCode::PageUp => {
                if let Some(object_key) = focused_key {
                    let view = self.state.dock_object_views.entry(object_key).or_default();
                    view.scroll = view.scroll.saturating_sub(5);
                }
            }
            KeyCode::PageDown => {
                if let Some(object_key) = focused_key {
                    let view = self.state.dock_object_views.entry(object_key).or_default();
                    view.scroll = view.scroll.saturating_add(5);
                }
            }
            KeyCode::Char('c') => {
                self.state.dock_pr_checkout_menu = Some(Default::default());
            }
            KeyCode::Char('l') => self.activate_dock_pr_action(
                crate::ui::work_list_detail::PrActionKind::Merge(self.state.pr_merge_method),
            ),
            KeyCode::Char('m') => self.state.dock_pr_action_menu = Some(Default::default()),
            KeyCode::Char('+') => {
                if let Some(object_key) = focused_key {
                    let overview = self
                        .state
                        .dock_object_views
                        .get(&object_key)
                        .is_none_or(|view| view.tab == crate::app::state::PrDetailTab::Overview);
                    if overview {
                        self.open_dock_pr_reviewer_picker(object_key);
                    }
                }
            }
            // Comments only render on the overview sub-tab, so the digits stay
            // free everywhere else.
            // A key with nothing to toggle stays unconsumed, exactly as it was
            // before these bindings existed.
            KeyCode::Char(digit @ '1'..='9') if self.dock_pr_comments_are_visible() => {
                let index = digit as usize - '1' as usize;
                if !focused_key
                    .is_some_and(|object_key| self.toggle_dock_detail_comment(object_key, index))
                {
                    return false;
                }
            }
            KeyCode::Char('a') if self.dock_pr_comments_are_visible() => {
                if !focused_key
                    .is_some_and(|object_key| self.toggle_all_dock_detail_comments(object_key))
                {
                    return false;
                }
            }
            KeyCode::Esc => self.state.dock_pr_focused = false,
            _ => return false,
        }
        true
    }

    /// Whether the focused pull-request detail is showing its comment list.
    fn dock_pr_comments_are_visible(&self) -> bool {
        crate::ui::dock::pr::focused_pr_key(&self.state).is_some_and(|object_key| {
            self.state
                .dock_object_views
                .get(&object_key)
                .is_none_or(|view| view.tab == crate::app::state::PrDetailTab::Overview)
        })
    }

    fn focused_dock_ticket_parts(
        &self,
    ) -> Option<(
        crate::app::state::WorkItemKey,
        crate::work_index::WorkTicket,
        String,
    )> {
        let key = crate::ui::dock::linear::focused_ticket_key(&self.state)?;
        let ticket_id = key.ticket_id.as_deref()?;
        let source = self
            .state
            .work_index_snapshot
            .as_ref()?
            .items
            .iter()
            .find(|item| {
                item.ticket_details
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(ticket_id))
            })?;
        let ticket = source
            .ticket_details
            .iter()
            .find(|ticket| ticket.identifier.eq_ignore_ascii_case(ticket_id))?
            .clone();
        Some((key, ticket, source.repo.clone()))
    }

    fn dock_ticket_action_context(&self) -> Option<crate::ui::ticket_actions::TicketActionContext> {
        let (key, ticket, _repo) = self.focused_dock_ticket_parts()?;
        Some(crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &ticket,
            self.state.work_item_detail_cache.get(&key),
            self.state.work_index_session.linear.viewer.as_deref(),
            self.state.work_index_session.linear_viewer_identity(),
            crate::ui::dock::pr::focused_pr_key(&self.state).is_some(),
        ))
    }

    fn handle_dock_linear_key(&mut self, key: &TerminalKey) -> bool {
        let previewed = self.state.dock_collapsed
            && self
                .state
                .dock_object_preview
                .as_ref()
                .is_some_and(|object| object.surface == crate::app::DockSurface::Linear);
        let dock_hosted = !self.state.dock_collapsed
            && self.state.dock_tab == Some(crate::app::DockSurface::Linear);
        if !(previewed || dock_hosted) || !self.state.dock_linear_focused {
            return false;
        }
        let event = key.as_key_event();
        // Alt digits fold sections; they are the only modified keys the detail
        // claims.
        if event.modifiers == crossterm::event::KeyModifiers::ALT {
            let KeyCode::Char(digit @ '1'..='9') = event.code else {
                return false;
            };
            // Same reasoning as the pull-request surface: a modal owning the
            // keyboard neither folds nor releases the key to the pane.
            if self.dock_linear_modal_owns_keyboard() {
                return true;
            }
            let area = dock_detail_area(&self.state);
            let layout = crate::ui::dock::linear::focused_ticket_layout(&self.state, area);
            let index = digit as usize - '1' as usize;
            return crate::ui::dock::linear::focused_ticket_key(&self.state).is_some_and(
                |object_key| self.toggle_dock_detail_section(object_key, layout, index),
            );
        }
        if !event.modifiers.is_empty() {
            return false;
        }
        if self.handle_pending_dock_write_key(event) {
            return true;
        }
        // No detail to act on: the surface is rendering its picker, so a digit
        // attaches a ticket. This mirrors the render gate exactly, including a
        // ticket that is bound but missing from the index. Esc still releases
        // focus; the detail bindings have no subject and must not fire.
        if crate::ui::dock::linear::focused_ticket_item(&self.state).is_none() {
            match event.code {
                KeyCode::Char(digit @ '1'..='9') => {
                    if let Some(ticket_id) =
                        crate::ui::dock::linear::attachable_ticket_for_digit(&self.state, digit)
                    {
                        self.attach_ticket_to_focused_pane(&ticket_id);
                    }
                }
                KeyCode::Esc => self.state.dock_linear_focused = false,
                _ => return false,
            }
            return true;
        }
        if let Some(draft) = self.state.dock_ticket_comment_draft.as_mut() {
            match event.code {
                KeyCode::Esc => self.state.dock_ticket_comment_draft = None,
                KeyCode::Backspace => {
                    draft.pop();
                }
                KeyCode::Enter => self.stage_dock_ticket_comment(),
                KeyCode::Char(character) => draft.push(character),
                _ => {}
            }
            return true;
        }
        if let Some(choice) = self.state.dock_ticket_start_menu {
            match event.code {
                KeyCode::Up | KeyCode::Down => {
                    self.state.dock_ticket_start_menu = Some(match choice {
                        crate::app::state::PrCheckoutChoice::CurrentCheckout => {
                            crate::app::state::PrCheckoutChoice::NewWorktree
                        }
                        crate::app::state::PrCheckoutChoice::NewWorktree => {
                            crate::app::state::PrCheckoutChoice::CurrentCheckout
                        }
                    });
                }
                KeyCode::Enter => self.open_dock_ticket_thread(choice),
                KeyCode::Esc => self.state.dock_ticket_start_menu = None,
                _ => {}
            }
            return true;
        }
        if let Some(mut menu) = self.state.dock_ticket_action_menu {
            let entries = self
                .dock_ticket_action_context()
                .map(|context| crate::ui::ticket_actions::ticket_action_table(&context, menu.page))
                .unwrap_or_default();
            match event.code {
                KeyCode::Up => {
                    menu.move_by(-1, entries.len());
                    self.state.dock_ticket_action_menu = Some(menu);
                }
                KeyCode::Down => {
                    menu.move_by(1, entries.len());
                    self.state.dock_ticket_action_menu = Some(menu);
                }
                KeyCode::Enter => {
                    if let Some(entry) = entries.get(menu.selected).filter(|entry| entry.enabled())
                    {
                        self.activate_dock_ticket_action(entry.action);
                    }
                }
                KeyCode::Esc => self.state.dock_ticket_action_menu = None,
                _ => {}
            }
            return true;
        }
        match event.code {
            KeyCode::PageUp => {
                if let Some(object_key) = crate::ui::dock::linear::focused_ticket_key(&self.state) {
                    let view = self.state.dock_object_views.entry(object_key).or_default();
                    view.scroll = view.scroll.saturating_sub(5);
                }
            }
            KeyCode::PageDown => {
                if let Some(object_key) = crate::ui::dock::linear::focused_ticket_key(&self.state) {
                    let view = self.state.dock_object_views.entry(object_key).or_default();
                    view.scroll = view.scroll.saturating_add(5);
                }
            }
            KeyCode::Char('c') => self.state.dock_ticket_start_menu = Some(Default::default()),
            KeyCode::Char('m') => self.state.dock_ticket_action_menu = Some(Default::default()),
            // A key with nothing to toggle stays unconsumed, exactly as it was
            // before these bindings existed.
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if !crate::ui::dock::linear::focused_ticket_key(&self.state)
                    .is_some_and(|object_key| self.toggle_dock_detail_comment(object_key, index))
                {
                    return false;
                }
            }
            KeyCode::Char('a') => {
                if !crate::ui::dock::linear::focused_ticket_key(&self.state)
                    .is_some_and(|object_key| self.toggle_all_dock_detail_comments(object_key))
                {
                    return false;
                }
            }
            KeyCode::Esc => self.state.dock_linear_focused = false,
            _ => return false,
        }
        true
    }

    fn open_dock_ticket_thread(&mut self, choice: crate::app::state::PrCheckoutChoice) {
        let Some((_key, ticket, repo)) = self.focused_dock_ticket_parts() else {
            return;
        };
        let prompt = Self::ticket_work_prompt(&ticket);
        self.state.dock_ticket_start_menu = None;
        self.open_ticket_home(ticket, repo, prompt, choice);
    }

    fn stage_dock_ticket_comment(&mut self) {
        let Some((_key, ticket, _repo)) = self.focused_dock_ticket_parts() else {
            return;
        };
        let Some(body) = self
            .state
            .dock_ticket_comment_draft
            .as_deref()
            .map(str::trim)
            .filter(|body| !body.is_empty())
            .map(str::to_string)
        else {
            return;
        };
        self.state.dock_ticket_comment_draft = None;
        self.state.dock_pending_write = Some(crate::work_index::WorkItemWrite::CommentOnTicket {
            identifier: ticket.identifier,
            body,
        });
        self.state.dock_write_notice = None;
    }

    fn activate_dock_ticket_action(&mut self, action: crate::ui::ticket_actions::TicketAction) {
        use crate::ui::ticket_actions::{TicketAction, TicketActionMenuPage};
        let Some((key, ticket, repo)) = self.focused_dock_ticket_parts() else {
            return;
        };
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &ticket,
            self.state.work_item_detail_cache.get(&key),
            self.state.work_index_session.linear.viewer.as_deref(),
            self.state.work_index_session.linear_viewer_identity(),
            crate::ui::dock::pr::focused_pr_key(&self.state).is_some(),
        );
        match action {
            TicketAction::TransitionMenu | TicketAction::PriorityMenu => {
                self.state.dock_ticket_action_menu =
                    Some(crate::ui::ticket_actions::TicketActionMenuState {
                        page: if action == TicketAction::TransitionMenu {
                            TicketActionMenuPage::Transitions
                        } else {
                            TicketActionMenuPage::Priorities
                        },
                        selected: 0,
                    });
            }
            TicketAction::Refresh => {
                self.state.dock_ticket_action_menu = None;
                self.next_work_index_refresh = std::time::Instant::now();
            }
            TicketAction::AskQuestion => self.open_ticket_home(
                ticket,
                repo,
                format!("About {} ({}): ", context.identifier, context.title),
                crate::app::state::PrCheckoutChoice::CurrentCheckout,
            ),
            TicketAction::Explain => self.open_ticket_home(
                ticket,
                repo,
                format!(
                    "Summarise {} ({}). Include the ticket, linked pull requests, and the acceptance criteria block.",
                    context.identifier, context.title
                ),
                crate::app::state::PrCheckoutChoice::CurrentCheckout,
            ),
            TicketAction::WorkInThread => {
                let prompt = Self::ticket_work_prompt(&ticket);
                self.open_ticket_home(
                    ticket,
                    repo,
                    prompt,
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::Transition(choice) => {
                self.state.dock_ticket_action_menu = None;
                self.state.dock_pending_write =
                    Some(crate::work_index::WorkItemWrite::TransitionTicket {
                        identifier: ticket.identifier,
                        state: choice.label().to_string(),
                    });
                self.state.dock_write_notice = None;
            }
            TicketAction::AssignToMe => {
                let Some(viewer) = context.viewer_id else {
                    return;
                };
                self.state.dock_ticket_action_menu = None;
                self.state.dock_pending_write =
                    Some(crate::work_index::WorkItemWrite::AssignTicket {
                        identifier: ticket.identifier,
                        assignee: viewer,
                    });
                self.state.dock_write_notice = None;
            }
            TicketAction::SetPriority(priority) => {
                self.state.dock_ticket_action_menu = None;
                self.state.dock_pending_write =
                    Some(crate::work_index::WorkItemWrite::SetTicketPriority {
                        identifier: ticket.identifier,
                        priority,
                    });
                self.state.dock_write_notice = None;
            }
            TicketAction::LinkPr => {
                let Some(pr) = crate::ui::dock::pr::focused_pr_key(&self.state) else {
                    return;
                };
                let (Some(number), Some(url)) = (pr.pr_number, pr.pr_url) else {
                    return;
                };
                self.state.dock_ticket_action_menu = None;
                self.state.dock_pending_write = Some(
                    crate::work_index::WorkItemWrite::LinkTicketPullRequest {
                        identifier: ticket.identifier,
                        title: format!("{}#{number}", pr.repo),
                        url,
                    },
                );
                self.state.dock_write_notice = None;
            }
            TicketAction::Comment => {
                self.state.dock_ticket_action_menu = None;
                self.state.dock_ticket_comment_draft = Some(String::new());
            }
            TicketAction::OpenInLinear => {
                self.state.dock_ticket_action_menu = None;
                if let Some(url) = context.url {
                    if let Err(error) = crate::platform::open_url(&url) {
                        self.state.dock_write_notice = Some(format!("could not open ticket: {error}"));
                    }
                }
            }
            TicketAction::CopyLink => {
                self.state.dock_ticket_action_menu = None;
                if let Some(url) = context.url {
                    self.state.request_clipboard_write = Some(url.into_bytes());
                }
            }
            TicketAction::CopyIdentifier => {
                self.state.dock_ticket_action_menu = None;
                self.state.request_clipboard_write = Some(ticket.identifier.into_bytes());
            }
            TicketAction::Cancel => {
                self.state.dock_ticket_action_menu = None;
                self.state.dock_pending_write =
                    Some(crate::work_index::WorkItemWrite::TransitionTicket {
                        identifier: ticket.identifier,
                        state: "Canceled".into(),
                    });
                self.state.dock_write_notice = None;
            }
        }
    }

    fn open_dock_pr_checkout(&mut self, choice: crate::app::state::PrCheckoutChoice) {
        let Some(key) = crate::ui::dock::pr::focused_pr_key(&self.state) else {
            return;
        };
        let Some(head) = self.pr_head_ref(&key) else {
            return;
        };
        let Some(number) = key.pr_number else {
            return;
        };
        let Some(url) = key.pr_url.clone() else {
            return;
        };
        let pr = crate::app::home::HomePrContext {
            url,
            number,
            repo: key.repo,
        };
        self.state.dock_pr_checkout_menu = None;
        self.open_pr_home(head, choice, String::new(), pr);
    }

    fn activate_dock_pr_action(&mut self, action: crate::ui::work_list_detail::PrActionKind) {
        let Some(key) = crate::ui::dock::pr::focused_pr_key(&self.state) else {
            return;
        };
        self.activate_pr_action(key, action);
    }

    fn open_selected_symphony_workflow(&mut self) {
        let Some(workflow) = self
            .state
            .symphony_detail
            .as_ref()
            .and_then(|detail| detail.snapshot.workflows.get(detail.selected))
            .cloned()
        else {
            return;
        };
        self.open_symphony_workflow(&workflow);
    }

    /// Open the job at `index` in the current snapshot.
    pub(crate) fn open_symphony_workflow_at(&mut self, index: usize) {
        let Some(workflow) = self.state.symphony_snapshot.workflows.get(index).cloned() else {
            return;
        };
        self.open_symphony_workflow(&workflow);
    }

    /// Open a job: the dock surface bound to it, and a terminal in its verified
    /// checkout. The surface opens even when the checkout cannot be resolved --
    /// what the job is doing is exactly what you want to read when it has
    /// nowhere to open.
    fn open_symphony_workflow(&mut self, workflow: &crate::symphony::Workflow) {
        let Some(repo) = workflow.repo.as_deref() else {
            self.state.bind_symphony_dock(workflow);
            self.state.config_diagnostic =
                Some("Symphony workflow has no repository checkout".to_string());
            return;
        };
        let mut verification_error = None;
        let workspace_match =
            self.state
                .workspaces
                .iter()
                .enumerate()
                .find_map(|(index, workspace)| {
                    let cwd = workspace.resolved_identity_cwd_from(
                        &self.state.terminals,
                        &self.terminal_runtimes,
                    )?;
                    match crate::symphony::checkout_matches_repo(&cwd, repo) {
                        Ok(()) => Some((index, cwd)),
                        Err(error) => {
                            verification_error.get_or_insert(error);
                            None
                        }
                    }
                });
        let (workspace_id, cwd) = if let Some((index, cwd)) = workspace_match {
            (Some(self.public_workspace_id(index)), Some(cwd))
        } else {
            match crate::symphony::common_checkout(repo) {
                Ok(cwd) => (None, cwd),
                Err(error) => {
                    verification_error.get_or_insert(error);
                    (None, None)
                }
            }
        };
        let Some(cwd) = cwd else {
            self.state.bind_symphony_dock(workflow);
            self.state.config_diagnostic = Some(
                verification_error
                    .unwrap_or_else(|| format!("Symphony checkout unavailable for {repo}")),
            );
            return;
        };
        self.runtime_tab_create(
            "tui.symphony.workflow.open",
            crate::api::schema::TabCreateParams {
                workspace_id,
                cwd: Some(cwd.to_string_lossy().into_owned()),
                focus: true,
                label: workflow
                    .ticket
                    .clone()
                    .or_else(|| Some(workflow.name.clone())),
                env: crate::symphony::launch_env(workflow),
                work_context: None,
            },
        );
        // The tab is focused now, but the dock still follows the pane the click
        // came from until the next reconcile. Bind after catching it up, or the
        // surface is saved under the old pane and the checkout opens without it.
        self.state.bind_symphony_dock_to_focused_pane(workflow);
        self.state.clear_symphony();
        self.state.set_server_mode(Mode::Terminal);
    }

    #[cfg(test)]
    pub(crate) fn handle_text_commit_headless(&mut self, text: &str) {
        self.handle_text_commit_headless_with_hook(text, &mut |_| {}, None);
    }

    #[cfg(test)]
    pub(crate) fn handle_text_commit_headless_with_hook(
        &mut self,
        text: &str,
        before_terminal_input: &mut impl FnMut(&super::TerminalInputTarget),
        controlled_owners: Option<&std::collections::HashMap<crate::terminal::TerminalId, u64>>,
    ) {
        let owner = self.state.input_owner();
        self.handle_text_commit_headless_for_owner_with_hook(
            owner,
            text,
            before_terminal_input,
            controlled_owners,
        );
    }

    pub(crate) fn handle_text_commit_headless_for_owner_with_hook(
        &mut self,
        owner: InputOwner,
        text: &str,
        before_terminal_input: &mut impl FnMut(&super::TerminalInputTarget),
        controlled_owners: Option<&std::collections::HashMap<crate::terminal::TerminalId, u64>>,
    ) {
        if text.is_empty() {
            return;
        }
        if owner == InputOwner::Popup {
            if let Some(runtime) = self.popup_runtime() {
                let _ = runtime.try_send_bytes(Bytes::copy_from_slice(text.as_bytes()));
            } else {
                self.close_popup_pane();
            }
            return;
        }
        if owner == InputOwner::Dock(DockInputOwner::Editor) {
            if let Some(runtime) = self.dock_editor_runtime() {
                let _ = runtime.try_send_bytes(Bytes::copy_from_slice(text.as_bytes()));
            }
            return;
        }
        if self.paste_into_input_owner(owner, text) {
            return;
        }
        if !owner.forwards_unhandled_input_to_pane() {
            return;
        }

        self.state.clear_selection();
        self.selection_autoscroll_deadline = None;
        self.state.update_dismissed = true;
        if let Some(ws_idx) = self.state.active {
            let pane_id = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.focused_pane_id());
            let target_terminal_id = pane_id.and_then(|pane_id| {
                self.state
                    .workspaces
                    .get(ws_idx)
                    .and_then(|workspace| workspace.terminal_id(pane_id).cloned())
            });
            if !text.is_empty() {
                if let Some(pane_id) = pane_id {
                    self.resume_settled_pane_before_input(pane_id);
                }
            }
            let sent = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
                .is_some_and(|runtime| {
                    if let Some(terminal_id) =
                        target_terminal_id.as_ref().filter(|_| !text.is_empty())
                    {
                        #[cfg(unix)]
                        if let Some(owner_id) =
                            controlled_owners.and_then(|owners| owners.get(terminal_id))
                        {
                            runtime.release_remote_owner(*owner_id);
                        }
                        #[cfg(not(unix))]
                        let _ = controlled_owners;
                        before_terminal_input(&super::TerminalInputTarget::new(
                            terminal_id.clone(),
                        ));
                    }
                    runtime
                        .try_send_bytes(Bytes::copy_from_slice(text.as_bytes()))
                        .is_ok()
                });
            if let (true, Some(pane_id)) = (sent, pane_id) {
                self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
                self.note_human_text(pane_id, text);
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn handle_text_commit(&mut self, text: String) {
        let owner = self.state.input_owner();
        self.handle_text_commit_for_input_owner(owner, text).await;
    }

    pub(super) async fn handle_text_commit_for_input_owner(
        &mut self,
        owner: InputOwner,
        text: String,
    ) {
        if text.is_empty() {
            return;
        }
        if owner == InputOwner::Popup {
            if let Some(runtime) = self.popup_runtime() {
                let _ = runtime.send_bytes(Bytes::from(text)).await;
            } else {
                self.close_popup_pane();
            }
            return;
        }
        if owner == InputOwner::Dock(DockInputOwner::Editor) {
            if let Some(runtime) = self.dock_editor_runtime() {
                let _ = runtime.send_bytes(Bytes::from(text)).await;
            }
            return;
        }
        if self.paste_into_input_owner(owner, &text) {
            return;
        }
        if !owner.forwards_unhandled_input_to_pane() {
            return;
        }

        self.state.clear_selection();
        self.selection_autoscroll_deadline = None;
        self.state.update_dismissed = true;
        if let Some(ws_idx) = self.state.active {
            let pane_id = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.focused_pane_id());
            if !text.is_empty() {
                if let Some(pane_id) = pane_id {
                    self.resume_settled_pane_before_input(pane_id);
                }
            }
            let sent = if let Some(runtime) = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            {
                runtime
                    .send_bytes(Bytes::copy_from_slice(text.as_bytes()))
                    .await
                    .is_ok()
            } else {
                false
            };
            if let (true, Some(pane_id)) = (sent, pane_id) {
                self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
                self.note_human_text(pane_id, &text);
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn handle_paste(&mut self, text: String) {
        let owner = self.state.input_owner();
        self.handle_paste_for_input_owner(owner, text).await;
    }

    pub(super) async fn handle_paste_for_input_owner(&mut self, owner: InputOwner, text: String) {
        if owner == InputOwner::Popup {
            if let Some(runtime) = self.popup_runtime() {
                let _ = runtime.send_paste(text).await;
            } else {
                self.close_popup_pane();
            }
            return;
        }
        if owner == InputOwner::Dock(DockInputOwner::Editor) {
            if let Some(runtime) = self.dock_editor_runtime() {
                let _ = runtime.send_paste(text).await;
            }
            return;
        }
        if self.paste_into_input_owner(owner, &text) || !owner.forwards_unhandled_input_to_pane() {
            return;
        }

        if let Some(ws_idx) = self.state.active {
            let pane_id = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.focused_pane_id());
            let draft = text.clone();
            let has_text = !text.is_empty();
            if has_text {
                if let Some(pane_id) = pane_id {
                    self.resume_settled_pane_before_input(pane_id);
                }
            }
            let sent = if let Some(runtime) = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            {
                runtime.send_paste(text).await.is_ok()
            } else {
                false
            };
            if let (true, Some(pane_id)) = (sent && has_text, pane_id) {
                self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
                self.note_human_text(pane_id, &draft);
            }
        }
    }

    pub(super) fn route_text_to_sidebar_subgroup_picker(&mut self, text: &str) -> bool {
        if self.state.sidebar_subgroup_picker.is_none() {
            return false;
        }
        for character in text.chars().filter(|character| !character.is_control()) {
            self.state.handle_sidebar_subgroup_picker_key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            ));
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn paste_into_active_text_input(&mut self, text: &str) -> bool {
        let owner = self.state.input_owner();
        self.paste_into_input_owner(owner, text)
    }

    pub(crate) fn paste_into_input_owner(&mut self, owner: InputOwner, text: &str) -> bool {
        match owner {
            InputOwner::Notepad => {
                // Read-only tabs own the input but have no buffer; pasting there
                // must not reach a note the user cannot see.
                if self.state.notepad.enabled
                    && matches!(
                        self.state.notepad.active_tab_target(),
                        Some(crate::notepad::NotepadTabTarget::Note(_))
                    )
                {
                    self.state
                        .notepad
                        .insert_text(text, std::time::Instant::now());
                }
                true
            }
            InputOwner::Client(ClientInputOwner::SnoozeTime) => {
                let Some(snooze) = self.state.sidebar_snooze.as_mut() else {
                    return false;
                };
                let Some(draft) = snooze.time_draft.as_mut() else {
                    return false;
                };
                snooze.error = None;
                draft.extend(text.chars().filter(|character| !character.is_control()));
                true
            }
            InputOwner::Client(ClientInputOwner::Overlay(
                crate::app::state::ClientOverlay::RenameWorkspace
                | crate::app::state::ClientOverlay::RenameTab
                | crate::app::state::ClientOverlay::RenamePane,
            )) => {
                insert_rename_input_text(&mut self.state, text);
                true
            }
            InputOwner::Client(ClientInputOwner::Overlay(
                crate::app::state::ClientOverlay::NewLinkedWorktree,
            )) => {
                self.insert_worktree_create_text(text);
                true
            }
            InputOwner::Client(ClientInputOwner::Overlay(
                crate::app::state::ClientOverlay::OpenExistingWorktree,
            )) => {
                if !self
                    .state
                    .worktree_open
                    .as_ref()
                    .is_some_and(|open| open.search_focused)
                {
                    return false;
                }
                self.insert_worktree_open_search_text(text);
                true
            }
            InputOwner::Client(ClientInputOwner::SidebarSubgroupPicker) => {
                self.route_text_to_sidebar_subgroup_picker(text)
            }
            InputOwner::Client(ClientInputOwner::SidebarPodPicker) => {
                for character in text.chars() {
                    self.handle_sidebar_pod_picker_key(KeyEvent::new(
                        KeyCode::Char(character),
                        KeyModifiers::empty(),
                    ));
                }
                true
            }
            InputOwner::AddProject | InputOwner::Surface(SurfaceInputOwner::Home) => {
                self.handle_home_text_commit(text);
                true
            }
            InputOwner::Surface(SurfaceInputOwner::Board) => self.board_insert_text(text),
            InputOwner::Server(ServerInputOwner::Navigator) => {
                if !self.state.navigator.search_focused {
                    return false;
                }
                insert_navigator_search_text(&mut self.state, &self.terminal_runtimes, text);
                true
            }
            InputOwner::Server(ServerInputOwner::CommandPalette) => {
                self.state.insert_command_palette_query_text(text);
                true
            }
            InputOwner::Server(ServerInputOwner::KeybindHelp) => {
                if !self.state.keybind_help.search_focused {
                    return false;
                }
                insert_keybind_help_query_text(&mut self.state, text);
                true
            }
            InputOwner::Server(ServerInputOwner::Copy) => {
                let Some(prompt) = self
                    .state
                    .copy_mode
                    .as_mut()
                    .and_then(|copy_mode| copy_mode.search.prompt.as_mut())
                else {
                    return false;
                };
                prompt
                    .query
                    .extend(text.chars().filter(|ch| !ch.is_control()));
                true
            }
            _ => false,
        }
    }

    pub(crate) fn paste_clipboard_shortcut_for_input_owner(
        &mut self,
        owner: InputOwner,
        key: &KeyEvent,
        read_clipboard: impl FnOnce() -> Option<String>,
    ) -> bool {
        is_modal_paste_shortcut(key)
            && read_clipboard().is_some_and(|text| self.paste_into_input_owner(owner, &text))
    }

    pub(crate) fn handle_onboarding_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Right | KeyCode::Char('l') => self.open_settings_from_onboarding(),
            _ => {
                if let Some(ModalAction::Continue) =
                    modal_action_from_key(&key, ONBOARDING_WELCOME_ACTIONS)
                {
                    self.open_settings_from_onboarding();
                }
            }
        }
    }

    pub(crate) fn handle_release_notes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_release_notes(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_release_notes(1),
            KeyCode::PageUp => self.scroll_release_notes(-8),
            KeyCode::PageDown => self.scroll_release_notes(8),
            KeyCode::Home => {
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.release_notes_max_scroll();
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = max_scroll;
                }
            }
            _ => {
                if let Some(ModalAction::Close) = modal_action_from_key(&key, RELEASE_NOTES_ACTIONS)
                {
                    self.dismiss_release_notes();
                }
            }
        }
    }

    pub(crate) fn handle_product_announcement_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_product_announcement(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_product_announcement(1),
            KeyCode::PageUp => self.scroll_product_announcement(-8),
            KeyCode::PageDown => self.scroll_product_announcement(8),
            KeyCode::Home => {
                if let Some(announcement) = &mut self.state.product_announcement {
                    announcement.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.product_announcement_max_scroll();
                if let Some(announcement) = &mut self.state.product_announcement {
                    announcement.scroll = max_scroll;
                }
            }
            _ => {
                if let Some(ModalAction::Close) = modal_action_from_key(&key, RELEASE_NOTES_ACTIONS)
                {
                    self.dismiss_product_announcement();
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        self.handle_mouse_from_input_source(super::LOCAL_INPUT_SOURCE, mouse);
    }

    #[cfg(test)]
    pub(super) fn handle_mouse_from_input_source(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
    ) {
        let presentation = crate::ui::pomodoro::input_presentation_at(
            &self.state,
            self.state.screen_rect(),
            std::time::Instant::now(),
        );
        self.handle_mouse_from_input_source_with_pomodoro_presentation(
            source_id,
            mouse,
            presentation,
        );
    }

    #[cfg(test)]
    pub(super) fn handle_mouse_from_input_source_with_pomodoro_presentation(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
        presentation: crate::ui::pomodoro::InputPresentation,
    ) {
        let owner = self
            .state
            .input_owner_with_pomodoro(presentation.prompt.is_some());
        self.handle_mouse_for_input_owner(source_id, mouse, presentation, owner);
    }

    pub(super) fn handle_mouse_for_input_owner(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
        presentation: crate::ui::pomodoro::InputPresentation,
        owner: InputOwner,
    ) {
        // A due break reminder is the topmost modal and must decide the click
        // before hover, pane focus, or any underlying control can react.
        if owner == InputOwner::Pomodoro {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some((confirm, snooze)) =
                    crate::ui::pomodoro::prompt_button_rects(presentation.area)
                {
                    let hit = |rect: ratatui::layout::Rect| {
                        mouse.column >= rect.x
                            && mouse.column < rect.right()
                            && mouse.row >= rect.y
                            && mouse.row < rect.bottom()
                    };
                    if hit(confirm) {
                        self.confirm_pomodoro(std::time::Instant::now());
                    } else if hit(snooze) {
                        self.state
                            .pomodoro
                            .dismiss_and_pause(std::time::Instant::now());
                    }
                }
            }
            return;
        }
        if self.handle_window_cycle_menu_mouse(mouse, owner) {
            return;
        }
        if self.handle_sidebar_areas_menu_mouse(mouse, owner) {
            return;
        }
        if let InputOwner::Client(owner) = owner {
            self.handle_client_mouse_for_input_owner(source_id, mouse, owner);
            return;
        }
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left)
                if self
                    .pending_config_diagnostic_click_sources
                    .contains(&source_id) =>
            {
                return;
            }
            MouseEventKind::Up(MouseButton::Left)
                if self
                    .pending_config_diagnostic_click_sources
                    .remove(&source_id) =>
            {
                self.state.clear_hovered_control();
                return;
            }
            _ => {}
        }
        if matches!(mouse.kind, MouseEventKind::Moved) {
            let hovered = crate::ui::hovered_control_at(&self.state, mouse.column, mouse.row);
            self.state
                .set_hovered_control_at(hovered, std::time::Instant::now());
        } else {
            self.state.clear_hovered_control();
        }
        if self.state.config_diagnostic.is_some()
            && self.state.point_in_rect(
                self.state.view.config_diagnostic_hit_area,
                mouse.column,
                mouse.row,
            )
            && matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.pending_config_diagnostic_click_sources
                    .insert(source_id);
            }
            self.state.config_diagnostic = None;
            self.config_diagnostic_deadline = None;
            return;
        }
        if owner == InputOwner::Surface(SurfaceInputOwner::Usage) {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let target = self
                    .state
                    .view
                    .usage_hit_areas
                    .iter()
                    .find(|hit| {
                        mouse.column >= hit.rect.x
                            && mouse.column < hit.rect.right()
                            && mouse.row >= hit.rect.y
                            && mouse.row < hit.rect.bottom()
                    })
                    .map(|hit| hit.target);
                if let Some(target) = target {
                    self.activate_usage_hit_target(target);
                    return;
                }
            }
            // Clicks outside the usage surface (tab bar, sidebar) keep their
            // normal handling so tabs stay clickable while the screen is open.
            if self
                .state
                .point_in_rect(self.state.view.terminal_area, mouse.column, mouse.row)
            {
                return;
            }
        }
        if owner == InputOwner::Surface(SurfaceInputOwner::Work) {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                && self.fold_work_view_section_at(mouse.column, mouse.row)
            {
                return;
            }
            self.handle_ticket_board_mouse(mouse);
            return;
        }
        if owner == InputOwner::Surface(SurfaceInputOwner::Symphony) {
            return;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.pending_url_click_sources.remove(&source_id);
            }
            MouseEventKind::Drag(MouseButton::Left)
                if self.pending_url_click_sources.contains(&source_id) =>
            {
                return;
            }
            MouseEventKind::Up(MouseButton::Left)
                if self.pending_url_click_sources.remove(&source_id) =>
            {
                return;
            }
            _ => {}
        }

        if owner == InputOwner::Popup {
            self.handle_popup_mouse(mouse);
            return;
        }
        if matches!(owner, InputOwner::Server(_)) && self.handle_overlay_mouse(mouse) {
            return;
        }

        if matches!(
            self.state.effective_interaction_mode(),
            Mode::Terminal | Mode::Navigate
        ) && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let notifications = self.state.view.notification_hit_area;
            if self
                .state
                .point_in_rect(notifications, mouse.column, mouse.row)
            {
                self.toggle_notifications();
                return;
            }
            let settings = self.state.view.sidebar_footer_settings_hit_area;
            if self.state.point_in_rect(settings, mouse.column, mouse.row) {
                settings::open_settings(&mut self.state);
                return;
            }
            let ask_subtitles = self.state.view.sidebar_footer_ask_subtitles_hit_area;
            if self
                .state
                .point_in_rect(ask_subtitles, mouse.column, mouse.row)
            {
                self.save_config_edit(crate::app::settings_general::ConfigEdit::Bool {
                    section: "ui.sidebar",
                    key: "show_ask_subtitles",
                    value: !self.state.sidebar_show_ask_subtitles,
                });
                return;
            }
            let work = self.state.view.sidebar_footer_work_hit_area;
            if mouse.column >= work.x
                && mouse.column < work.x.saturating_add(work.width)
                && mouse.row >= work.y
                && mouse.row < work.y.saturating_add(work.height)
            {
                self.toggle_work_view();
                return;
            }
            let tickets = self.state.view.sidebar_footer_ticket_hit_area;
            if mouse.column >= tickets.x
                && mouse.column < tickets.x.saturating_add(tickets.width)
                && mouse.row >= tickets.y
                && mouse.row < tickets.y.saturating_add(tickets.height)
            {
                self.toggle_ticket_view();
                return;
            }
            let usage = self.state.view.sidebar_footer_usage_hit_area;
            if mouse.column >= usage.x
                && mouse.column < usage.right()
                && mouse.row >= usage.y
                && mouse.row < usage.bottom()
            {
                self.toggle_usage_view();
                return;
            }
            let missive = self.state.view.sidebar_footer_missive_hit_area;
            if mouse.column >= missive.x
                && mouse.column < missive.right()
                && mouse.row >= missive.y
                && mouse.row < missive.bottom()
            {
                self.toggle_missive_view();
                return;
            }
            let refresh = self.state.view.sidebar_footer_refresh_hit_area;
            let board = self.state.view.sidebar_footer_board_hit_area;
            if mouse.column >= board.x
                && mouse.column < board.right()
                && mouse.row >= board.y
                && mouse.row < board.bottom()
            {
                self.toggle_board_view();
                return;
            }
            if mouse.column >= refresh.x
                && mouse.column < refresh.right()
                && mouse.row >= refresh.y
                && mouse.row < refresh.bottom()
            {
                self.request_sidebar_refresh();
                return;
            }

            if let Some(copy_value) = self
                .state
                .view
                .work_context_link_rows
                .iter()
                .find(|row| {
                    mouse.column >= row.rect.x
                        && mouse.column < row.rect.x.saturating_add(row.rect.width)
                        && mouse.row >= row.rect.y
                        && mouse.row < row.rect.y.saturating_add(row.rect.height)
                })
                .map(|row| row.copy_value.clone())
            {
                if self
                    .event_tx
                    .try_send(crate::events::AppEvent::ClipboardWrite {
                        content: copy_value.into_bytes(),
                    })
                    .is_err()
                {
                    self.show_work_link_notice("could not copy work link");
                } else {
                    self.show_work_link_notice("copied");
                }
                return;
            }

            // A named work link opens its object where the human asked for it:
            // the click is the opt-in the dock no longer takes on its own.
            if let Some(object) = self
                .state
                .view
                .status_work_links
                .iter()
                .find(|link| {
                    mouse.column >= link.rect.x
                        && mouse.column < link.rect.x.saturating_add(link.rect.width)
                        && mouse.row >= link.rect.y
                        && mouse.row < link.rect.y.saturating_add(link.rect.height)
                })
                .map(|link| link.object.clone())
            {
                self.state.dock_collapsed = false;
                self.state
                    .open_dock_object(object, crate::app::state::DockTabOrigin::User);
                return;
            }

            if let Some(action) = self
                .state
                .view
                .status_buttons
                .iter()
                .find(|button| {
                    mouse.column >= button.rect.x
                        && mouse.column < button.rect.x.saturating_add(button.rect.width)
                        && mouse.row >= button.rect.y
                        && mouse.row < button.rect.y.saturating_add(button.rect.height)
                })
                .map(|button| button.action)
            {
                self.activate_status_button(action);
                return;
            }

            // Scratchpad rows open rather than copy: the note is being read, and
            // the reason a link is in it is to be followed.
            if let Some(url) = self
                .state
                .view
                .scratchpad_link_rows
                .iter()
                .find(|row| {
                    mouse.column >= row.rect.x
                        && mouse.column < row.rect.x.saturating_add(row.rect.width)
                        && mouse.row >= row.rect.y
                        && mouse.row < row.rect.y.saturating_add(row.rect.height)
                })
                .map(|row| row.url.clone())
            {
                if let Err(error) = crate::platform::open_url(&url) {
                    tracing::warn!(%error, %url, "failed to open scratchpad link");
                    self.show_work_link_notice("could not open link");
                }
                return;
            }
        }

        if self.state.board_view.is_some()
            && matches!(
                owner,
                InputOwner::Surface(SurfaceInputOwner::Board)
                    | InputOwner::Sidebar
                    | InputOwner::Pane
            )
            && self
                .state
                .point_in_rect(self.state.view.terminal_area, mouse.column, mouse.row)
        {
            self.handle_board_mouse(mouse);
            return;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_sidebar_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let is_double_click = self
                .last_sidebar_divider_click
                .is_some_and(|last| now.duration_since(last) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW);
            self.last_sidebar_divider_click = Some(now);

            if is_double_click {
                self.state.sidebar_width = self.state.default_sidebar_width;
                self.state.sidebar_width_source =
                    crate::app::state::SidebarWidthSource::ConfigDefault;
                self.state.sidebar_width_auto = false;
                self.state.mark_session_dirty();
                self.state.drag = None;
                return;
            }
        }

        if self.handle_modified_url_click(source_id, mouse) {
            return;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_dock_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let double_click = self.last_dock_divider_click.is_some_and(|previous| {
                now.duration_since(previous) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW
            });
            self.last_dock_divider_click = Some(now);
            if double_click {
                self.state.set_dock_width(crate::ui::DOCK_DEFAULT_WIDTH);
                self.state.drag = None;
            } else {
                self.state.drag = Some(crate::app::state::DragState {
                    target: crate::app::state::DragTarget::DockDivider,
                });
            }
            return;
        }

        if owner == InputOwner::AddProject {
            self.state
                .handle_mouse_for_owner(&mut self.terminal_runtimes, source_id, mouse, owner);
            self.start_home_github_refresh_if_requested();
            return;
        }

        let editor_preview_hit = self.state.dock_editor_preview.is_some()
            && self
                .state
                .point_in_rect(self.state.view.terminal_area, mouse.column, mouse.row);
        let handled_pane_double_click = !editor_preview_hit && self.handle_pane_double_click(mouse);
        if owner == InputOwner::Pane && !handled_pane_double_click && !editor_preview_hit {
            self.focus_pane_before_mouse_press(mouse);
        }

        let previous_agent_panel_sort = self.state.agent_panel_sort;
        let previous_settings_section = self.state.settings.section;
        if !handled_pane_double_click {
            let action = self.state.handle_mouse_for_owner(
                &mut self.terminal_runtimes,
                source_id,
                mouse,
                owner,
            );
            self.start_home_ref_refresh_if_requested();
            self.start_home_github_refresh_if_requested();
            // Mouse reports can be forwarded to the terminal, but they do not
            // answer a blocked closing gate. Only explicit human text/key input
            // retires its authority.
            let _ = self.state.take_forwarded_pane_input();
            if let Some(action) = action {
                match action {
                    MouseAction::SidebarObjectMenu { index } => {
                        self.apply_sidebar_object_menu_action(index)
                    }
                    MouseAction::SettledMenu { index } => {
                        self.apply_sidebar_settled_menu_action(index)
                    }
                    MouseAction::SnoozeMenu { action } => {
                        self.apply_sidebar_snooze_menu_action(action)
                    }
                    MouseAction::AgentPickerSelect(index) => {
                        if let Ok(digit) = u8::try_from(index.saturating_add(1)) {
                            if let Some(digit) = b'0'.checked_add(digit).map(char::from) {
                                self.handle_agent_picker_key(KeyEvent::new(
                                    KeyCode::Char(digit),
                                    KeyModifiers::empty(),
                                ));
                            }
                        }
                    }
                    MouseAction::FocusLiveSettledPane(target) => self.focus_settled_pane(target),
                    MouseAction::OpenSnoozeMenu {
                        target,
                        column,
                        row,
                    } => self.open_sidebar_snooze_menu(target, column, row),
                    MouseAction::SettlePane(target) => self.settle_sidebar_pane(target),
                    MouseAction::SidebarNewMenu { action } => {
                        if action == crate::app::state::SidebarNewMenuAction::NewSpace {
                            self.begin_tui_workspace_create("tui.mouse.workspace.create");
                        } else {
                            self.state.dispatch_sidebar_new_menu_action(action);
                        }
                    }
                    MouseAction::NewWorkspace => {
                        self.begin_tui_workspace_create("tui.mouse.workspace.create")
                    }
                    MouseAction::DispatchSidebarWork(plan) => {
                        self.dispatch_sidebar_work_group_plan(*plan)
                    }
                    MouseAction::Settings(action) => self.apply_settings_action(action),
                    // Home is a full-terminal overlay, but the sidebar sits
                    // outside it and its clicks fall through. Picking a session
                    // there used to move focus behind the overlay, leaving home
                    // covering the pane the user had just chosen.
                    MouseAction::FocusWorkspace { ws_idx } => {
                        self.state.clear_home();
                        self.focus_workspace_idx_via_api(ws_idx)
                    }
                    MouseAction::FocusTab { tab_idx } => {
                        self.state.clear_home();
                        self.focus_tab_idx_via_api(tab_idx)
                    }
                    MouseAction::FocusPane { ws_idx, pane_id } => {
                        self.state.clear_home();
                        self.focus_pane_internal_via_api(ws_idx, pane_id)
                    }
                    MouseAction::OpenUrl { url } => {
                        if let Err(error) = crate::platform::open_url(&url) {
                            self.state.config_diagnostic =
                                Some(format!("Could not open {url}: {error}"));
                        }
                    }
                    MouseAction::OpenSymphonyWorkflow { index } => {
                        self.state.clear_home();
                        self.open_symphony_workflow_at(index);
                    }
                    MouseAction::OpenAgentRunLog { host, run_id } => {
                        self.state.clear_home();
                        self.open_agent_run_log(&host, &run_id);
                    }
                    MouseAction::OpenFleetHost { name, focus_agent } => {
                        self.open_fleet_host_from_input(&name, focus_agent.as_deref());
                    }
                    MouseAction::FocusToastTarget => {
                        self.state.clear_home();
                        self.focus_toast_target_via_api()
                    }
                    MouseAction::DockTicketAction { action } => {
                        self.activate_dock_ticket_action(action)
                    }
                    MouseAction::DockTicketStartThread { choice } => {
                        self.open_dock_ticket_thread(choice)
                    }
                    MouseAction::RefreshDockFiles => self.force_dock_files_refresh(),
                    MouseAction::SortDockFiles => self.state.cycle_dock_files_sort(),
                    MouseAction::PreviewDockFile(path) => self.preview_dock_file(path),
                    MouseAction::OpenDockFile(path) => self.open_dock_file_in_editor(path),
                    MouseAction::RefreshEditorPreview => self.refresh_dock_editor_preview(),
                    MouseAction::OpenEditorPreview => self.open_dock_editor_preview_in_editor(),
                    MouseAction::MoveWorkspace {
                        source_ws_idx,
                        insert_idx,
                    } => self.move_workspace_via_api(source_ws_idx, insert_idx),
                    MouseAction::MoveWorkspaceBlock { params } => {
                        self.move_workspace_block_via_api(params)
                    }
                    MouseAction::MoveTab {
                        ws_idx,
                        source_tab_idx,
                        insert_idx,
                    } => self.move_tab_via_api(ws_idx, source_tab_idx, insert_idx),
                    MouseAction::SetPaneGroup(params) => self.runtime_pane_group_set(params),
                    MouseAction::PodPickerSelect(index) => self.accept_sidebar_pod_picker(index),
                    MouseAction::SetSplitRatio { path, ratio } => {
                        self.set_split_ratio_via_api(path, ratio)
                    }
                    MouseAction::RenameModal(action) => {
                        self.apply_rename_mouse_action_via_api(action)
                    }
                    MouseAction::ConfirmCloseAccept => self.confirm_close_accept_via_api(),
                    MouseAction::ContextMenu { menu, action } => {
                        let menu = *menu;
                        self.state.close_client_overlay();
                        self.apply_context_menu_action_via_api(menu, action)
                    }
                }
            }
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                && self
                    .state
                    .selection
                    .as_ref()
                    .is_none_or(crate::selection::Selection::is_in_progress)
            {
                self.selection_highlight_clear_deadline = None;
            }
        }
        self.finish_mouse_handling(previous_agent_panel_sort, previous_settings_section);
    }

    fn handle_client_mouse_for_input_owner(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
        owner: ClientInputOwner,
    ) {
        let previous_agent_panel_sort = self.state.agent_panel_sort;
        let previous_settings_section = self.state.settings.section;
        let action = match owner {
            ClientInputOwner::PrActionConfirmation => {
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                    if let Some((cancel, confirm)) =
                        crate::ui::pr_actions::confirmation_button_rects(
                            &self.state,
                            self.state.screen_rect(),
                        )
                    {
                        let hit = |rect: ratatui::layout::Rect| {
                            mouse.column >= rect.x
                                && mouse.column < rect.right()
                                && mouse.row >= rect.y
                                && mouse.row < rect.bottom()
                        };
                        if hit(confirm) {
                            if let Some(confirmation) = self.state.pr_action_confirmation.take() {
                                self.execute_pr_action_confirmation(confirmation);
                            }
                        } else if hit(cancel) {
                            self.state.pr_action_confirmation = None;
                        }
                    }
                }
                return;
            }
            ClientInputOwner::SnoozeTime => self.state.handle_mouse_for_owner(
                &mut self.terminal_runtimes,
                source_id,
                mouse,
                InputOwner::Client(ClientInputOwner::SnoozeTime),
            ),
            ClientInputOwner::Overlay(overlay) => self.state.handle_mouse_for_owner(
                &mut self.terminal_runtimes,
                source_id,
                mouse,
                InputOwner::Client(ClientInputOwner::Overlay(overlay)),
            ),
            ClientInputOwner::SnoozeMenu
            | ClientInputOwner::SettledMenu
            | ClientInputOwner::SettledDeleteConfirm
            | ClientInputOwner::AgentPicker
            | ClientInputOwner::SidebarGroupMenu
            | ClientInputOwner::SidebarFilterMenu
            | ClientInputOwner::SidebarNewMenu
            | ClientInputOwner::SidebarNewThread
            | ClientInputOwner::SidebarProjectMenu
            | ClientInputOwner::SidebarObjectMenu
            | ClientInputOwner::SidebarSortMenu
            | ClientInputOwner::SidebarSubgroupPicker
            | ClientInputOwner::SidebarPodPicker
            | ClientInputOwner::DockSurfaceMenu => self.state.handle_mouse_for_owner(
                &mut self.terminal_runtimes,
                source_id,
                mouse,
                InputOwner::Client(owner),
            ),
        };
        self.start_home_ref_refresh_if_requested();
        self.start_home_github_refresh_if_requested();
        if let Some(action) = action {
            match action {
                MouseAction::SidebarObjectMenu { index } => {
                    self.apply_sidebar_object_menu_action(index)
                }
                MouseAction::SettledMenu { index } => self.apply_sidebar_settled_menu_action(index),
                MouseAction::SnoozeMenu { action } => self.apply_sidebar_snooze_menu_action(action),
                MouseAction::AgentPickerSelect(index) => {
                    if let Ok(digit) = u8::try_from(index.saturating_add(1)) {
                        if let Some(digit) = b'0'.checked_add(digit).map(char::from) {
                            self.handle_agent_picker_key(KeyEvent::new(
                                KeyCode::Char(digit),
                                KeyModifiers::empty(),
                            ));
                        }
                    }
                }
                MouseAction::SidebarNewMenu { action } => {
                    if action == crate::app::state::SidebarNewMenuAction::NewSpace {
                        self.begin_tui_workspace_create("tui.mouse.workspace.create");
                    } else {
                        self.state.dispatch_sidebar_new_menu_action(action);
                    }
                }
                MouseAction::RenameModal(action) => self.apply_rename_mouse_action_via_api(action),
                MouseAction::SetPaneGroup(params) => self.runtime_pane_group_set(params),
                MouseAction::PodPickerSelect(index) => self.accept_sidebar_pod_picker(index),
                MouseAction::ConfirmCloseAccept => self.confirm_close_accept_via_api(),
                MouseAction::ContextMenu { menu, action } => {
                    let menu = *menu;
                    self.state.close_client_overlay();
                    self.apply_context_menu_action_via_api(menu, action);
                }
                _ => unreachable!("client input owner returned a generic mouse action"),
            }
        }
        self.finish_mouse_handling(previous_agent_panel_sort, previous_settings_section);
    }

    fn finish_mouse_handling(
        &mut self,
        previous_agent_panel_sort: crate::app::state::AgentPanelSort,
        previous_settings_section: crate::app::state::SettingsSection,
    ) {
        if previous_settings_section != crate::app::state::SettingsSection::Integrations
            && self.state.settings.section == crate::app::state::SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
        self.start_requested_tool_probes();
        if self.state.agent_panel_sort != previous_agent_panel_sort {
            self.save_agent_panel_sort(self.state.agent_panel_sort);
        }
        self.dispatch_pending_clipboard_write();
        if self.state.selection_autoscroll.is_none() {
            self.selection_autoscroll_deadline = None;
        } else if self.selection_autoscroll_deadline.is_none() {
            self.selection_autoscroll_deadline =
                Some(std::time::Instant::now() + super::SELECTION_AUTOSCROLL_INTERVAL);
        }
    }

    fn handle_ticket_board_mouse(&mut self, mouse: MouseEvent) {
        let Some(view) = self.state.work_view.as_ref() else {
            return;
        };
        if view.projection != crate::app::state::WorkProjection::Tickets {
            return;
        }
        let area = self.state.view.terminal_area;
        let list_toggle = ratatui::layout::Rect::new(area.x + 10.min(area.width), area.y, 6, 1);
        let board_toggle = ratatui::layout::Rect::new(area.x + 17.min(area.width), area.y, 7, 1);
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if self
                .state
                .point_in_rect(list_toggle, mouse.column, mouse.row)
            {
                if let Some(view) = self.state.work_view.as_mut() {
                    view.ticket_layout = crate::app::state::LinearViewLayout::List;
                    view.board_detail_open = false;
                }
                return;
            }
            if self
                .state
                .point_in_rect(board_toggle, mouse.column, mouse.row)
            {
                if let Some(view) = self.state.work_view.as_mut() {
                    view.ticket_layout = crate::app::state::LinearViewLayout::Board;
                    view.board_detail_open = false;
                }
                return;
            }
        }
        if view.ticket_layout != crate::app::state::LinearViewLayout::Board
            || view.board_detail_open
        {
            return;
        }
        let layout = crate::ui::work_view::ticket_board_layout(&self.state, view, area);
        let hit = layout
            .cards
            .iter()
            .find(|hit| self.state.point_in_rect(hit.rect, mouse.column, mouse.row));
        let Some(hit) = hit.cloned() else { return };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let now = std::time::Instant::now();
                let double_click = view.board_last_click.as_ref().is_some_and(|(key, at)| {
                    key == &hit.key
                        && now
                            .checked_duration_since(*at)
                            .is_some_and(|age| age <= std::time::Duration::from_millis(500))
                });
                if let Some(view) = self.state.work_view.as_mut() {
                    view.board_column = hit.column;
                    view.board_rows[hit.column] = hit.row;
                    view.selected = Some(hit.key.clone());
                    view.board_detail_open = double_click;
                    view.board_last_click = Some((hit.key, now));
                }
            }
            MouseEventKind::ScrollUp => {
                self.move_ticket_board_row(-1);
            }
            MouseEventKind::ScrollDown => {
                self.move_ticket_board_row(1);
            }
            _ => {}
        }
    }

    fn handle_popup_mouse(&mut self, mouse: MouseEvent) {
        let Some((_outer, inner)) =
            crate::ui::popup_pane_rects(&self.state, self.state.view.terminal_area)
        else {
            return;
        };
        if mouse.column < inner.x
            || mouse.column >= inner.x.saturating_add(inner.width)
            || mouse.row < inner.y
            || mouse.row >= inner.y.saturating_add(inner.height)
        {
            return;
        }
        let Some(rt) = self.popup_runtime() else {
            self.close_popup_pane();
            return;
        };
        let position = crate::input::mouse::Position::Cell {
            column: mouse.column.saturating_sub(inner.x),
            row: mouse.row.saturating_sub(inner.y),
        };
        let bytes = match mouse.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => match rt.wheel_routing() {
                Some(crate::pane::WheelRouting::MouseReport) => {
                    rt.encode_mouse_wheel(mouse.kind, position, mouse.modifiers)
                }
                Some(crate::pane::WheelRouting::AlternateScroll) => {
                    rt.encode_alternate_scroll(mouse.kind)
                }
                Some(crate::pane::WheelRouting::HostScroll) | None => {
                    let lines_per_notch = self.state.mouse_scroll_lines;
                    match mouse.kind {
                        MouseEventKind::ScrollUp => rt.scroll_up(lines_per_notch),
                        MouseEventKind::ScrollDown => rt.scroll_down(lines_per_notch),
                        _ => {}
                    }
                    return;
                }
            },
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
                rt.encode_mouse_button(mouse.kind, position, mouse.modifiers)
            }
            MouseEventKind::Moved => rt.encode_mouse_motion(mouse.kind, position, mouse.modifiers),
        };
        let Some(bytes) = bytes else {
            return;
        };
        if !matches!(mouse.kind, MouseEventKind::Moved) {
            rt.scroll_reset();
        }
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(err = %err, kind = ?mouse.kind, "failed to forward popup mouse event");
        }
    }

    fn focus_pane_before_mouse_press(&mut self, mouse: MouseEvent) {
        if !matches!(
            self.state.effective_interaction_mode(),
            Mode::Terminal | Mode::Resize
        ) || !matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Middle)
        ) {
            return;
        }

        let Some(pane_id) = self
            .state
            .pane_at(mouse.column, mouse.row)
            .map(|info| info.id)
        else {
            return;
        };
        let Some(ws_idx) = self.state.active else {
            return;
        };

        self.state.dock_editor_focused = false;
        self.state.dock_home_focused = false;
        self.state.dock_diff_focused = false;
        self.state.dock_files_focused = false;
        self.state.dock_agents_focused = false;
        self.state.dock_hosts_focused = false;
        // Focus through the runtime API before an application can consume its press.
        self.focus_pane_internal_via_api(ws_idx, pane_id);
    }

    fn handle_modified_url_click(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
    ) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.handle_modified_url_click_with(source_id, mouse, |url| {
                crate::platform::open_url(url).map(|()| None)
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.handle_modified_url_click_with(source_id, mouse, crate::platform::open_url)
        }
    }

    fn handle_modified_url_click_with(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
        open_url: impl FnOnce(&str) -> std::io::Result<Option<std::process::Child>>,
    ) -> bool {
        if self.state.effective_interaction_mode() != Mode::Terminal
            || !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            || !mouse.modifiers.contains(modified_url_click_modifier())
        {
            return false;
        }

        let Some(info) = self.state.pane_at(mouse.column, mouse.row).cloned() else {
            return false;
        };
        let viewport_row = mouse.row.saturating_sub(info.inner_rect.y);
        let col = mouse.column.saturating_sub(info.inner_rect.x);
        let Some(url) =
            self.state
                .url_at_pane_cell(&self.terminal_runtimes, info.id, viewport_row, col)
        else {
            return false;
        };

        if !self.open_pane_link_from(&url, Some(info.id), open_url) {
            return false;
        }

        self.last_pane_click = None;
        self.pending_url_click_sources.insert(source_id);
        true
    }

    /// Open a link a pane printed, the way a modified click on it would.
    pub(crate) fn open_pane_link(&mut self, url: String) {
        #[cfg(target_os = "macos")]
        self.open_pane_link_from(&url, None, |url| {
            crate::platform::open_url(url).map(|()| None)
        });
        #[cfg(not(target_os = "macos"))]
        self.open_pane_link_from(&url, None, crate::platform::open_url);
    }

    /// Plugins get first refusal, because a link handler exists to redirect
    /// links Herdr would otherwise hand to a browser. Returns whether anything
    /// took the link.
    fn open_pane_link_from(
        &mut self,
        url: &str,
        pane_id: Option<crate::layout::PaneId>,
        open_url: impl FnOnce(&str) -> std::io::Result<Option<std::process::Child>>,
    ) -> bool {
        let plugin_handled = match pane_id
            .map(|pane_id| self.invoke_plugin_link_handler_for_url(url, pane_id))
            .transpose()
        {
            Ok(handled) => handled.unwrap_or(false),
            Err(err) => {
                tracing::warn!(err = %err, url = %url, "failed to invoke plugin link handler");
                false
            }
        };
        if plugin_handled {
            return true;
        }
        if crate::app::actions::safe_web_url(url).is_none() {
            return false;
        }
        match open_url(url) {
            Ok(Some(child)) => self.detached_process_children.push(child),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(err = %err, url = %url, "failed to open pane URL");
            }
        }
        true
    }

    fn handle_pane_double_click(&mut self, mouse: MouseEvent) -> bool {
        // A pane press stops being a double-click candidate once it becomes
        // a drag or completes as a real text selection.
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                self.last_pane_click = None;
                return false;
            }
            MouseEventKind::Up(MouseButton::Left)
                if self
                    .state
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.is_visible()) =>
            {
                self.last_pane_click = None;
                return false;
            }
            _ => {}
        }

        // Only terminal-pane left-clicks can start this gesture; other clicks
        // should keep their existing mouse behavior and clear stale candidates.
        let Some(click) = self.pane_click_candidate(mouse) else {
            return false;
        };

        // Require the second click to land near the first click in the same pane
        // and within the double-click window so adjacent interactions do not select a word.
        if !self.take_pane_double_click(click) {
            return false;
        }

        self.select_double_clicked_word(click)
    }

    fn pane_click_candidate(&mut self, mouse: MouseEvent) -> Option<PaneClickState> {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }

        if !mouse.modifiers.is_empty() {
            self.last_pane_click = None;
            return None;
        }

        if self.state.effective_interaction_mode() != Mode::Terminal {
            self.last_pane_click = None;
            return None;
        }

        let Some(info) = self.state.pane_at(mouse.column, mouse.row).cloned() else {
            self.last_pane_click = None;
            return None;
        };

        Some(PaneClickState {
            pane_id: info.id,
            viewport_row: mouse.row - info.inner_rect.y,
            col: mouse.column - info.inner_rect.x,
            at: std::time::Instant::now(),
        })
    }

    fn take_pane_double_click(&mut self, click: PaneClickState) -> bool {
        if !self
            .last_pane_click
            .is_some_and(|last| last.is_double_click_for(click))
        {
            self.last_pane_click = Some(click);
            return false;
        }

        self.last_pane_click = None;
        true
    }

    fn select_double_clicked_word(&mut self, click: PaneClickState) -> bool {
        let selected = self.state.select_word_at_pane_cell(
            &self.terminal_runtimes,
            click.pane_id,
            click.viewport_row,
            click.col,
        );
        if selected {
            self.selection_highlight_clear_deadline = self
                .state
                .copy_on_select
                .then(|| std::time::Instant::now() + super::PANE_COPY_HIGHLIGHT_DURATION);
        }
        selected
    }
}

/// The area the focused dock detail is drawn into. A collapsed dock previews
/// its object over the terminal area instead of the dock body.
pub(crate) fn dock_detail_area(state: &AppState) -> ratatui::layout::Rect {
    if state.dock_collapsed {
        state.view.terminal_area
    } else {
        state.view.dock_body_rect
    }
}

/// The foldable section whose header rule the screen `row` lands on.
///
/// The render scrolls the paragraph, and clamps that scroll to the content
/// height, so the same clamp has to be applied here or a click near the bottom
/// of a short detail resolves to the wrong line.
fn section_at_row(
    layout: &crate::ui::work_view::DetailLayout,
    view: &crate::app::state::ObjectViewState,
    area: ratatui::layout::Rect,
    row: u16,
) -> Option<crate::app::state::DetailSection> {
    let max_scroll = layout.lines.len().saturating_sub(usize::from(area.height));
    let scroll = usize::from(view.scroll).min(max_scroll);
    let line = scroll.checked_add(usize::from(row.checked_sub(area.y)?))?;
    layout
        .sections
        .iter()
        .find(|(header, _)| *header == line)
        .map(|(_, section)| *section)
}

/// The identity the work-view render matches a selection on: a ticket by its
/// identifier, a pull request by repository and number.
fn same_work_object(
    left: &crate::app::state::WorkItemKey,
    right: &crate::app::state::WorkItemKey,
) -> bool {
    match (left.ticket_id.as_deref(), right.ticket_id.as_deref()) {
        (Some(left), Some(right)) => left == right,
        (None, None) => left.repo == right.repo && left.pr_number == right.pr_number,
        _ => false,
    }
}

pub(crate) fn is_modal_paste_shortcut(key: &KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('v' | 'V')) {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
    }

    #[cfg(not(target_os = "macos"))]
    {
        key.modifiers.contains(KeyModifiers::CONTROL)
    }
}

#[cfg(test)]
pub(crate) fn modal_paste_target_active(state: &AppState) -> bool {
    if state.notepad.focused {
        return true;
    }
    if state
        .sidebar_snooze
        .as_ref()
        .is_some_and(|snooze| snooze.time_draft.is_some())
    {
        return true;
    }
    match state.effective_interaction_mode() {
        Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane | Mode::NewLinkedWorktree => {
            true
        }
        Mode::OpenExistingWorktree => state
            .worktree_open
            .as_ref()
            .is_some_and(|open| open.search_focused),
        Mode::Navigator => state.navigator.search_focused,
        Mode::CommandPalette => true,
        Mode::KeybindHelp => state.keybind_help.search_focused,
        Mode::Copy => state
            .copy_mode
            .as_ref()
            .is_some_and(|copy_mode| copy_mode.search.prompt.is_some()),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Mouse handling
// ---------------------------------------------------------------------------

// Note: split_pane needs runtime (event_tx for PTY spawn), so it lives on App
impl AppState {
    #[cfg(test)]
    pub(crate) fn split_pane(
        &mut self,
        terminal_runtimes: &mut crate::terminal::TerminalRuntimeRegistry,
        direction: Direction,
    ) {
        self.split_pane_with_placement(terminal_runtimes, direction, false);
    }

    #[cfg(test)]
    pub(crate) fn split_pane_with_placement(
        &mut self,
        terminal_runtimes: &mut crate::terminal::TerminalRuntimeRegistry,
        direction: Direction,
        before: bool,
    ) {
        // Actual PTY spawning happens in Workspace::split_focused
        // which needs events channel — this is called from navigate_key
        // where we don't have async context, so the workspace handles it
        let (rows, cols) = self.estimate_pane_size();
        let new_rows = (rows / 2).max(4);
        let new_cols = (cols / 2).max(10);

        let follow_cwd = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .and_then(|ws| {
                let tab = ws.active_tab()?;
                let terminal_id = tab.terminal_id(tab.layout.focused())?;
                super::creation::launch_cwd_for_terminal(
                    terminal_id,
                    &self.terminals,
                    terminal_runtimes,
                )
            });
        let cwd = Some(super::creation::resolve_new_terminal_cwd(
            &self.new_terminal_cwd,
            follow_cwd,
        ));

        let previous_focus = self.current_pane_focus_target();
        let pane_terminal_theme = self.pane_terminal_theme();
        let pane_terminal_appearance = Some(self.pane_terminal_appearance());
        if let Some(ws_idx) = self.active {
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            let split = if before {
                ws.split_focused_with_placement(
                    direction,
                    true,
                    new_rows,
                    new_cols,
                    cwd,
                    self.pane_scrollback_limit_bytes,
                    pane_terminal_theme,
                    pane_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&self.default_shell, self.shell_mode),
                    Vec::new(),
                )
            } else {
                ws.split_focused(
                    direction,
                    new_rows,
                    new_cols,
                    cwd,
                    self.pane_scrollback_limit_bytes,
                    pane_terminal_theme,
                    pane_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&self.default_shell, self.shell_mode),
                    Vec::new(),
                )
            };
            if let Ok(new_pane) = split {
                let new_id = new_pane.pane_id;
                terminal_runtimes.insert(new_pane.terminal.id.clone(), new_pane.runtime);
                self.remove_alias_shadowed_by_new_pane(new_id);
                self.terminals
                    .insert(new_pane.terminal.id.clone(), new_pane.terminal);
                self.record_pane_focus_change(previous_focus, ws_idx, new_id);
                self.mark_session_dirty();
                self.set_server_mode(Mode::Terminal);
            }
        }
    }
}

#[cfg(test)]
fn state_with_workspaces(names: &[&str]) -> AppState {
    let mut state = AppState::test_new();
    state.workspaces = names
        .iter()
        .map(|name| crate::workspace::Workspace::test_new(name))
        .collect();
    if !state.workspaces.is_empty() {
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(Mode::Navigate);
    }
    state
}

#[cfg(test)]
fn app_for_mouse_test() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        true,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.set_server_mode(Mode::Terminal);
    app.state.sidebar_collapsed = false;
    // Keep bottom-left dock geometry stable for input tests. Sidebar panel
    // defaults are covered by tests that load `Config::default()` directly.
    app.state.notepad.set_visible_tabs(Vec::new());
    // Deliberately not the shipped default (`Hidden`): these tests click on a
    // tab row, so they need one.
    app.state.tab_bar_position = crate::config::TabBarPositionConfig::Top;
    app.state.update_available = None;
    app.state.latest_release_notes_available = false;
    app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 26, 20);
    app.state.view.terminal_area = ratatui::layout::Rect::new(26, 0, 80, 20);
    app
}

#[cfg(test)]
fn mouse(
    kind: crossterm::event::MouseEventKind,
    col: u16,
    row: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::empty(),
    }
}

#[cfg(test)]
fn numbered_lines_bytes(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| format!("{i:06}\r\n"))
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
fn capture_snapshot(state: &AppState) -> crate::persist::SessionSnapshot {
    let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &terminal_runtimes,
        state.active,
        state.selected,
        state.sidebar_width,
        state.sidebar_section_split,
        state.collapsed_space_keys.clone(),
        state.prio_panel_collapsed,
        state.window_cycle_mode,
        state.skip_collapsed_cycle,
    )
}

#[cfg(test)]
fn root_layout_ratio(snapshot: &crate::persist::SessionSnapshot) -> Option<f32> {
    match &snapshot.workspaces.first()?.tabs.first()?.layout {
        crate::persist::LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
        crate::persist::LayoutSnapshot::Pane(_) => None,
    }
}

#[cfg(test)]
fn unique_temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
}

#[cfg(test)]
#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.is_empty() {
                return content;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

#[cfg(test)]
#[cfg(target_os = "linux")]
async fn wait_for_detached_process_reap(app: &mut App, pid: u32) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while crate::platform::process_exists(pid) && tokio::time::Instant::now() < deadline {
        app.reap_finished_detached_processes();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    app.reap_finished_detached_processes();
    !crate::platform::process_exists(pid)
}

#[cfg(test)]
#[cfg(unix)]
async fn wait_for_custom_command_reap(app: &mut App, pid: u32) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while crate::platform::process_exists(pid) && tokio::time::Instant::now() < deadline {
        app.reap_finished_custom_commands();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    app.reap_finished_custom_commands();
    !crate::platform::process_exists(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dispatch_starts_on_the_plans_remote_machine() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.home = Some(crate::app::home::HomeState::default());
        let plan = crate::app::home::HomeDispatchPlan {
            agent: crate::detect::Agent::Codex,
            model: "gpt-5.6-sol".into(),
            effort: Some("high".into()),
            directory: std::env::temp_dir(),
            workspace: crate::app::home::HomeWorkspace::CurrentCheckout,
            git_ref: None,
            pr: None,
            ticket: None,
            missive: None,
            work_context_patch: crate::work_context::PaneWorkContextPatch::default(),
            target: crate::app::home::HomeTarget::NewSpace,
            prompt: "repair the pending finding".into(),
            argv: vec!["agent-that-must-not-start-locally".into()],
            env: Vec::new(),
            remote: Some(crate::app::machines::Machine {
                name: "producer".into(),
                icon: None,
                target: Some("--invalid-target".into()),
                socket: Some("/run/herdr-producer.sock".into()),
            }),
        };

        app.dispatch_home_plan(plan.clone())
            .expect("remote dispatch should start");

        assert!(app.state.workspaces.is_empty(), "must not launch locally");
        assert_eq!(
            app.state
                .home
                .as_ref()
                .and_then(|home| home.pending_dispatch.as_ref()),
            Some(&plan),
            "the producer-host launch must remain pending"
        );
    }

    fn hidden_sidebar_config_app() -> App {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-hidden-sidebar-input-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("create config fixture directory");
        let config_path = directory.join("config.toml");
        std::fs::write(
            &config_path,
            r#"
onboarding = false

[keys]
prefix = "ctrl+a"

[ui]
sidebar_width = 42
sidebar_min_width = 24
sidebar_max_width = 120
sidebar_collapsed_mode = "hidden"
mouse_capture = true

[notepad]
enabled = true

[pomodoro]
enabled = true
sidebar_visible = true
"#,
        )
        .expect("write config fixture");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &config_path);
        let loaded = crate::config::Config::load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        let mut app = App::new(
            &loaded.config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.set_server_mode(Mode::Terminal);
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("ub1")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.dock_width = 82;
        app.state.sidebar_group_mode = crate::app::state::SidebarGroupMode::Repo;
        app.state.sidebar_work_filter = crate::app::state::SidebarWorkFilter::default();
        std::fs::remove_dir_all(directory).expect("remove config fixture directory");
        app
    }

    fn compute_hidden_sidebar(app: &mut App) {
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 30));
    }

    #[test]
    fn hidden_sidebar_config_toggles_through_raw_key_and_mouse_input() {
        let key_encodings = [
            (vec![0x01], b"B".to_vec()),
            (b"\x1b[97;5u".to_vec(), b"\x1b[98;2u".to_vec()),
            (b"\x1b[97;5u".to_vec(), b"\x1b[98:66;2u".to_vec()),
            (b"\x1b[97;5u".to_vec(), b"\x1b[66;1u".to_vec()),
            (b"\x1b[97;5u".to_vec(), b"\x1b[66;2u".to_vec()),
        ];

        for (prefix, rhs) in key_encodings {
            let mut app = hidden_sidebar_config_app();
            compute_hidden_sidebar(&mut app);
            assert_eq!(app.state.view.sidebar_rect.width, 42);

            app.route_client_input(prefix.clone());
            app.route_client_input(rhs.clone());
            compute_hidden_sidebar(&mut app);
            assert!(app.state.sidebar_collapsed, "failed encoding: {rhs:?}");
            assert_eq!(
                app.state.view.sidebar_rect.width, 0,
                "failed encoding: {rhs:?}"
            );

            app.route_client_input(prefix);
            app.route_client_input(rhs.clone());
            compute_hidden_sidebar(&mut app);
            assert!(!app.state.sidebar_collapsed, "failed encoding: {rhs:?}");
            assert_eq!(
                app.state.view.sidebar_rect.width, 42,
                "failed encoding: {rhs:?}"
            );
        }

        let mut app = hidden_sidebar_config_app();
        app.route_client_input(vec![0x01]);
        app.route_client_input(b"b".to_vec());
        assert!(!app.state.sidebar_collapsed);

        let mut app = hidden_sidebar_config_app();
        compute_hidden_sidebar(&mut app);
        let sidebar = app.state.view.sidebar_rect;
        let toggle = crate::ui::expanded_sidebar_toggle_rect(sidebar);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))
            .expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app.state, frame))
            .expect("render ub1 sidebar");

        let mouse = format!("\x1b[<0;{};{}M", toggle.x + 1, toggle.y + 1);
        app.route_client_input(mouse.into_bytes());
        compute_hidden_sidebar(&mut app);
        assert!(app.state.sidebar_collapsed);
        assert_eq!(app.state.view.sidebar_rect.width, 0);

        app.route_client_input(vec![0x01]);
        app.route_client_input(b"B".to_vec());
        compute_hidden_sidebar(&mut app);
        assert!(!app.state.sidebar_collapsed);
        assert_eq!(app.state.view.sidebar_rect.width, 42);
    }

    #[test]
    fn due_break_prompt_blocks_then_releases_hidden_sidebar_toggles() {
        let mut app = hidden_sidebar_config_app();
        let now = std::time::Instant::now();
        app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );

        app.route_client_input(vec![0x01]);
        app.route_client_input(b"B".to_vec());
        compute_hidden_sidebar(&mut app);
        assert!(app.state.sidebar_collapsed);
        assert_eq!(app.state.view.sidebar_rect.width, 0);

        app.state
            .pomodoro
            .tick(now + std::time::Duration::from_secs(25 * 60));
        assert!(app.state.pomodoro.prompt.is_some());

        app.route_client_input(vec![0x01]);
        app.route_client_input(b"B".to_vec());
        compute_hidden_sidebar(&mut app);
        assert!(
            app.state.sidebar_collapsed,
            "due prompt owns the toggle key"
        );

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))
            .expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app.state, frame))
            .expect("render due prompt over hidden sidebar");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("time for a break"), "{rendered:?}");
        assert!(rendered.contains("↵ confirm"), "{rendered:?}");
        assert!(rendered.contains("^⌥b snooze"), "{rendered:?}");

        for key in b"tea" {
            app.route_client_input(vec![*key]);
        }
        app.route_client_input(b"\r".to_vec());
        assert!(app.state.pomodoro.prompt.is_none());
        assert!(app.state.pomodoro.send_off.is_some());
        app.state
            .pomodoro
            .dismiss_send_off_at(std::time::Instant::now());

        app.route_client_input(vec![0x01]);
        app.route_client_input(b"B".to_vec());
        compute_hidden_sidebar(&mut app);
        assert!(
            !app.state.sidebar_collapsed,
            "toggle key works after answer"
        );

        let toggle = crate::ui::expanded_sidebar_toggle_rect(app.state.view.sidebar_rect);
        let mouse = format!("\x1b[<0;{};{}M", toggle.x + 1, toggle.y + 1);
        app.route_client_input(mouse.into_bytes());
        compute_hidden_sidebar(&mut app);
        assert!(
            app.state.sidebar_collapsed,
            "toggle icon works after answer"
        );
        assert_eq!(app.state.view.sidebar_rect.width, 0);
    }

    #[test]
    fn due_break_prompt_buttons_confirm_and_snooze_through_raw_mouse_input() {
        fn prompted_app() -> App {
            let mut app = hidden_sidebar_config_app();
            let now = std::time::Instant::now();
            app.state.pomodoro = crate::pomodoro::PomodoroState::from_config(
                &crate::config::PomodoroConfig {
                    enabled: true,
                    ..Default::default()
                },
                now,
            );
            app.state
                .pomodoro
                .tick(now + std::time::Duration::from_secs(25 * 60));
            compute_hidden_sidebar(&mut app);
            app
        }

        let mut app = prompted_app();
        for key in b"tea" {
            app.route_client_input(vec![*key]);
        }
        let (confirm, _) = crate::ui::pomodoro::prompt_button_rects(app.state.screen_rect())
            .expect("prompt buttons");
        let mouse = format!("\x1b[<0;{};{}M", confirm.x + 1, confirm.y + 1);
        app.route_client_input(mouse.into_bytes());
        assert!(app.state.pomodoro.prompt.is_none());
        assert!(app.state.pomodoro.send_off.is_some());

        let mut app = prompted_app();
        let (_, snooze) = crate::ui::pomodoro::prompt_button_rects(app.state.screen_rect())
            .expect("prompt buttons");
        let mouse = format!("\x1b[<0;{};{}M", snooze.x + 1, snooze.y + 1);
        app.route_client_input(mouse.into_bytes());
        assert!(app.state.pomodoro.prompt.is_none());
        assert!(app.state.pomodoro.send_off.is_none());
        assert!(app.state.pomodoro.paused());
    }

    #[tokio::test]
    async fn send_off_key_closes_the_card_without_reaching_the_pane() {
        let mut app = hidden_sidebar_config_app();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, mut pane_input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.workspaces[0].insert_test_runtime(pane_id, runtime);
        let started = std::time::Instant::now();
        app.state.pomodoro.reset(started);
        app.state
            .pomodoro
            .tick(started + std::time::Duration::from_secs(25 * 60));
        for key in b"tea" {
            app.route_client_input(vec![*key]);
        }
        app.route_client_input(b"\r".to_vec());
        assert!(app.state.pomodoro.send_off.is_some());
        compute_hidden_sidebar(&mut app);

        app.route_client_input(b"x".to_vec());

        assert!(app.state.pomodoro.send_off.is_none());
        assert!(
            pane_input.try_recv().is_err(),
            "the closing key must not reach the pane"
        );
    }

    #[tokio::test]
    async fn headless_raw_text_paste_and_uncaptured_click_close_send_off_without_pane_input() {
        fn app_with_send_off(mouse_capture: bool) -> (App, tokio::sync::mpsc::Receiver<Bytes>) {
            let mut app = hidden_sidebar_config_app();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let (runtime, pane_input) =
                crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                    80,
                    24,
                    0,
                    b"\x1b[?1003h\x1b[?1006h",
                    16,
                );
            assert!(
                runtime.mouse_reporting_enabled(),
                "reproduction requires a mouse-reporting pane"
            );
            app.state.workspaces[0].insert_test_runtime(pane_id, runtime);
            app.state.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
                started: crate::pomodoro::PomodoroPhase::ShortBreak,
                shown_at: std::time::Instant::now(),
            });
            app.state.mouse_capture = mouse_capture;
            compute_hidden_sidebar(&mut app);
            (app, pane_input)
        }

        for event in [
            crate::raw_input::RawInputEvent::Text(crate::input::TextCommit::new("committed")),
            crate::raw_input::RawInputEvent::Paste("pasted".into()),
        ] {
            let (mut app, mut pane_input) = app_with_send_off(false);
            app.route_client_events(vec![event], true);

            assert!(app.state.pomodoro.send_off.is_none());
            assert!(
                pane_input.try_recv().is_err(),
                "the closing raw input must not reach the pane"
            );
        }

        for mouse_capture in [false, true] {
            let (mut app, mut pane_input) = app_with_send_off(mouse_capture);
            let pane = app
                .state
                .view
                .pane_infos
                .iter()
                .find(|pane| pane.is_focused)
                .expect("focused pane");
            let mouse = |kind| {
                crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                    kind,
                    column: pane.inner_rect.x,
                    row: pane.inner_rect.y,
                    modifiers: KeyModifiers::empty(),
                })
            };
            app.route_client_events(
                vec![
                    mouse(MouseEventKind::Moved),
                    mouse(MouseEventKind::ScrollDown),
                    mouse(MouseEventKind::Down(MouseButton::Left)),
                    mouse(MouseEventKind::Drag(MouseButton::Left)),
                    mouse(MouseEventKind::Up(MouseButton::Left)),
                ],
                true,
            );
            assert!(app.state.pomodoro.send_off.is_none());
            assert!(
                pane_input.try_recv().is_err(),
                "send-off mouse events reached the pane with capture={mouse_capture}"
            );
        }

        let (mut app, mut pane_input) = app_with_send_off(false);
        app.state.pomodoro.send_off = None;
        let pane = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.is_focused)
            .expect("focused pane");
        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: pane.inner_rect.x,
                row: pane.inner_rect.y,
                modifiers: KeyModifiers::empty(),
            })],
            true,
        );
        assert!(
            pane_input.try_recv().is_ok(),
            "without the send-off, the same click reaches the mouse-reporting pane"
        );

        let (mut app, mut pane_input) = app_with_send_off(false);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 18, 30));
        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Text(
                crate::input::TextCommit::new("visible-pane"),
            )],
            true,
        );

        assert!(app.state.pomodoro.send_off.is_some());
        assert_eq!(
            pane_input.recv().await.expect("text reaches pane").as_ref(),
            b"visible-pane"
        );
    }

    #[test]
    fn send_off_click_closes_the_card_without_activating_the_ui_below() {
        let mut app = hidden_sidebar_config_app();
        let started = std::time::Instant::now();
        app.state.pomodoro.reset(started);
        app.state
            .pomodoro
            .tick(started + std::time::Duration::from_secs(25 * 60));
        app.state.pomodoro.prompt.as_mut().expect("prompt").input = "tea".into();
        app.confirm_pomodoro(started + std::time::Duration::from_secs(25 * 60));
        assert!(app.state.pomodoro.send_off.is_some());
        compute_hidden_sidebar(&mut app);
        let timer = app.state.view.pomodoro_hit_area;
        assert!(timer.width > 0, "timer control is below the send-off");
        assert!(app.state.pomodoro.running());

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: timer.x,
                row: timer.y,
                modifiers: KeyModifiers::empty(),
            })],
            true,
        );

        assert!(app.state.pomodoro.send_off.is_none());
        assert!(
            app.state.pomodoro.running(),
            "the timer click was swallowed"
        );
    }

    fn terminal_app_with_blocked_hook() -> (
        App,
        crate::terminal::TerminalId,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
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
        (app, terminal_id, rx)
    }

    fn terminal_app_with_mouse_reporting_blocked_hook(
        mouse_modes: &[u8],
    ) -> (
        App,
        crate::terminal::TerminalId,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                mouse_modes,
                16,
            );
        assert!(runtime.mouse_reporting_enabled());
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        let session_ref =
            crate::agent_resume::AgentSessionRef::id("pi-mouse-input").expect("Pi session ref");
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: session_ref.clone(),
        });
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            crate::detect::AgentState::Blocked,
            None,
            Some(session_ref),
            Some(1),
            std::time::Instant::now(),
        );
        assert!(terminal.blocked_state_hold_active_for_test());
        assert!(terminal.full_lifecycle_hook_authority_active());
        (app, terminal_id, rx)
    }

    fn assert_blocked_hook_held(app: &App, terminal_id: &crate::terminal::TerminalId) {
        assert_eq!(
            app.state.terminals[terminal_id].raw_agent_state(),
            crate::detect::AgentState::Blocked
        );
        assert!(app.state.terminals[terminal_id].full_lifecycle_hook_authority_active());
    }

    fn assert_blocked_hook_retired(app: &App, terminal_id: &crate::terminal::TerminalId) {
        assert_eq!(
            app.state.terminals[terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );
        assert!(!app.state.terminals[terminal_id].full_lifecycle_hook_authority_active());
    }

    fn test_app() -> App {
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn pasting_on_a_read_only_tab_leaves_the_hidden_note_untouched() {
        let mut app = test_app();
        app.state.notepad.enabled = true;
        app.state
            .notepad
            .set_files(vec![crate::notepad::NotepadFile {
                path: "/notes/hidden.md".into(),
                name: "hidden".into(),
            }]);
        app.state.notepad.set_visible_tabs(vec!["usage".into()]);
        assert_eq!(
            app.state.notepad.active_tab_target(),
            Some(crate::notepad::NotepadTabTarget::Usage)
        );

        assert!(app.paste_into_input_owner(InputOwner::Notepad, "secret"));

        assert!(!app.state.notepad.dirty);
    }

    #[tokio::test]
    async fn escape_closes_aloop_run_log_through_surface_input_owner() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::Terminal);
        app.state.show_aloop_run_detail(
            "nightly".into(),
            "ub2".into(),
            std::sync::Arc::new(crate::aloop::RunRecord {
                at: "2026-09-18T09:59:00Z".into(),
                at_unix_s: 1_758_186_740,
                duration_ms: 1_200,
                exit: 0,
                findings: 0,
                stable_ids: Vec::new(),
                log_excerpt: "clean log tail".into(),
            }),
        );

        assert_eq!(
            app.state.input_owner(),
            InputOwner::Surface(SurfaceInputOwner::AloopRunLog)
        );
        assert!(app.terminal_input_context().is_none());

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert!(app.state.aloop_run_detail.is_none());
    }

    /// Both Aloops views are reachable only from the sidebar, so a fixture that
    /// opens one has to leave the sidebar focused the way the real path does.
    fn aloop_sidebar_app() -> App {
        let mut app = test_app();
        app.state.set_server_mode(Mode::Terminal);
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            aloop: Some(crate::aloop::ProducerSnapshot::read(
                "ub2".to_string(),
                crate::aloop::HostData {
                    loops: vec![crate::aloop::LoopRuns {
                        loop_name: "nightly".to_string(),
                        runs: vec![std::sync::Arc::new(crate::aloop::RunRecord {
                            at: "2026-09-18T09:59:00Z".to_string(),
                            at_unix_s: crate::fleet::parse_utc_timestamp("2026-09-18T09:59:00Z")
                                .expect("timestamp"),
                            duration_ms: 1_200,
                            exit: 0,
                            findings: 0,
                            stable_ids: Vec::new(),
                            log_excerpt: "clean log tail".to_string(),
                        })],
                        skipped_lines: 0,
                    }],
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 30, 40);
        // The section and the clean-run fold are both closed on a fresh state,
        // so a click test has to open them the way the operator does.
        app.state
            .toggle_sidebar_group(crate::ui::sidebar::ALOOPS_SECTION_TITLE);
        app.state.toggle_sidebar_group("aloop-clean:nightly");
        app.state.focus_client_on_sidebar();
        app
    }

    /// Walks the laid-out sidebar rows the way a click does, so the mouse tests
    /// never hardcode a row offset that a row-order change would invalidate.
    fn aloop_row_position(app: &crate::app::state::AppState, key_prefix: &str) -> (u16, u16) {
        use crate::ui::sidebar::aloops::AloopTarget;
        let rect = app.view.sidebar_rect;
        // Six columns in clears the fold glyph, which owns the first three.
        let col = rect.x.saturating_add(6);
        for row in rect.y..rect.y + rect.height {
            let Some(target) = crate::ui::sidebar_aloop_target_at(app, col, row) else {
                continue;
            };
            let key = match &target {
                AloopTarget::Finding { key }
                | AloopTarget::RunLine { key }
                | AloopTarget::CleanFold { key }
                | AloopTarget::Loop { key, .. }
                | AloopTarget::CleanRun { key, .. } => key.as_str(),
            };
            if key.starts_with(key_prefix) {
                return (col, row);
            }
        }
        panic!("no aloops row matching {key_prefix}");
    }

    fn left_click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    // MAT-168 AC1/AC4: opening the run log from the sidebar has to hand input
    // ownership to the view, or Esc stays with the sidebar and never closes it.
    #[tokio::test]
    async fn ac1_escape_closes_the_aloop_run_log_opened_from_the_sidebar() {
        let mut app = aloop_sidebar_app();
        app.state.sidebar_selected_work_group =
            Some("aloop:cleanrun:nightly:2026-09-18T09:59:00Z".into());

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert!(
            app.state.aloop_run_detail.is_some(),
            "the sidebar Enter path opens the run log"
        );
        assert_eq!(
            app.state.input_owner(),
            InputOwner::Surface(SurfaceInputOwner::AloopRunLog)
        );

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert!(app.state.aloop_run_detail.is_none());
    }

    // MAT-168 AC2/AC4.
    #[tokio::test]
    async fn ac2_escape_closes_the_aloop_loop_history_opened_from_the_sidebar() {
        let mut app = aloop_sidebar_app();
        app.state.sidebar_selected_work_group = Some("aloop:loop:nightly".into());

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert!(
            app.state.loop_run_history_detail.is_some(),
            "the sidebar Enter path opens the loop history"
        );
        assert_eq!(
            app.state.input_owner(),
            InputOwner::Surface(SurfaceInputOwner::LoopRunHistory)
        );

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert!(app.state.loop_run_history_detail.is_none());
    }

    // MAT-168 AC3: the click path opens the same two views and keeps the
    // sidebar selection, so it needs the same ownership handover.
    #[tokio::test]
    async fn ac3_escape_closes_the_aloop_run_log_opened_by_mouse() {
        let mut app = aloop_sidebar_app();
        let (column, row) = aloop_row_position(&app.state, "aloop:cleanrun:nightly");

        app.handle_mouse(left_click(column, row));

        assert!(
            app.state.aloop_run_detail.is_some(),
            "clicking a clean run opens the run log"
        );
        assert_eq!(
            app.state.input_owner(),
            InputOwner::Surface(SurfaceInputOwner::AloopRunLog)
        );

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert!(app.state.aloop_run_detail.is_none());
    }

    // MAT-168 AC3.
    #[tokio::test]
    async fn ac3_escape_closes_the_aloop_loop_history_opened_by_mouse() {
        let mut app = aloop_sidebar_app();
        let (column, row) = aloop_row_position(&app.state, "aloop:loop:nightly");

        app.handle_mouse(left_click(column, row));

        assert!(
            app.state.loop_run_history_detail.is_some(),
            "clicking a loop header opens the loop history"
        );
        assert_eq!(
            app.state.input_owner(),
            InputOwner::Surface(SurfaceInputOwner::LoopRunHistory)
        );

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert!(app.state.loop_run_history_detail.is_none());
    }

    fn pr_action_test_app() -> (App, crate::app::state::WorkItemKey) {
        let mut app = test_app();
        let key = crate::app::state::WorkItemKey {
            repo: "owner/repo".into(),
            pr_number: Some(7),
            pr_url: Some("https://github.com/owner/repo/pull/7".into()),
            ticket_id: None,
        };
        let item = crate::work_index::WorkItem {
            repo: key.repo.clone(),
            pr_number: key.pr_number,
            pr_url: key.pr_url.clone(),
            pr_title: Some("repair parser".into()),
            pr_state: Some("open".into()),
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 1,
            deletions: 0,
            author: Some("ada".into()),
            assignees: vec!["ada".into()],
            labels: vec!["bug".into()],
            check_state: crate::work_index::PrCheckState::Passing,
            audience: crate::work_index::PrAudience::Authored,
            cached_pr_detail: None,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: Some("fix/parser".into()),
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: Default::default(),
        };
        app.state.work_view = Some(crate::app::state::WorkViewState::new(
            true,
            Some(crate::work_index::Snapshot {
                items: vec![item],
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: std::time::SystemTime::now(),
            }),
        ));
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.head_ref_name = Some("fix/parser".into());
        detail.comments = vec![crate::work_index::WorkItemComment {
            author: Some("issue-author".into()),
            body: "ordinary issue comment".into(),
            created_at: None,
        }];
        app.state.work_item_detail_cache.insert(key.clone(), detail);
        (app, key)
    }

    #[test]
    fn switching_pr_tabs_uses_cached_object_without_scheduling_refetch() {
        let (mut app, key) = pr_action_test_app();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 120, 40);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(60);
        app.next_work_index_refresh = later;
        let cached_comments = app
            .state
            .work_item_detail_cache
            .get(&key)
            .map(|detail| detail.comments.clone());

        assert!(app.handle_work_view_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty(),)));

        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .map(|view| view.object_view(&key).tab),
            Some(crate::app::state::PrDetailTab::Files)
        );
        assert_eq!(app.next_work_index_refresh, later);
        assert_eq!(app.work_index_cache_bypass, Default::default());
        assert_eq!(
            app.state
                .work_item_detail_cache
                .get(&key)
                .map(|detail| detail.comments.clone()),
            cached_comments
        );
    }

    #[test]
    fn chooser_shortcuts_open_available_surfaces_and_ignore_the_rest() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_open_surfaces.clear();
        app.state.dock_tab = None;
        app.state.dock_chooser_focused = true;

        // Uppercase arrives with shift; the card label is uppercase, so both
        // spellings of the same shortcut have to work.
        assert!(
            app.handle_dock_chooser_key(&TerminalKey::new(KeyCode::Char('T'), KeyModifiers::SHIFT))
        );
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Terminal));

        app.state.dock_open_surfaces.clear();
        app.state.dock_tab = None;
        app.state.dock_chooser_focused = true;
        assert!(app
            .handle_dock_chooser_key(&TerminalKey::new(KeyCode::Char('f'), KeyModifiers::empty())));
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Files));

        for (key, surface) in [
            ('h', crate::app::DockSurface::Home),
            ('e', crate::app::DockSurface::Editor),
            ('k', crate::app::DockSurface::Shortcuts),
            ('x', crate::app::DockSurface::Context),
            ('n', crate::app::DockSurface::Scratchpad),
        ] {
            app.state.dock_open_surfaces.clear();
            app.state.dock_tab = None;
            app.state.dock_chooser_focused = true;
            assert!(app.handle_dock_chooser_key(&TerminalKey::new(
                KeyCode::Char(key),
                KeyModifiers::empty(),
            )));
            assert_eq!(app.state.dock_tab, Some(surface), "shortcut {key}");
        }

        // No pull request on the focused pane: the card is inert, and the key
        // travels on to whatever would have had it.
        app.state.dock_open_surfaces.clear();
        app.state.dock_tab = None;
        app.state.dock_chooser_focused = true;
        assert!(app
            .handle_dock_chooser_key(&TerminalKey::new(KeyCode::Char('p'), KeyModifiers::empty())));
        assert_eq!(app.state.dock_tab, None);

        // An unrelated key is not swallowed by the chooser.
        assert!(!app
            .handle_dock_chooser_key(&TerminalKey::new(KeyCode::Char('z'), KeyModifiers::empty())));
    }

    #[test]
    fn shifted_surface_shortcut_switches_an_already_open_focused_dock() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.cached_git_space = crate::workspace::git_space_metadata(
            &std::env::current_dir().expect("current test directory"),
        );
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.dock_home_focused = true;

        assert!(app
            .handle_dock_chooser_key(&TerminalKey::new(KeyCode::Char('D'), KeyModifiers::SHIFT,)));
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Diff));
        assert!(app.state.dock_diff_focused);
    }

    #[test]
    fn one_escape_restores_a_maximised_dock() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Scratchpad);
        app.state.dock_maximized = true;

        assert!(app.handle_dock_chooser_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(!app.state.dock_maximized);
        assert_eq!(
            app.state.dock_tab,
            Some(crate::app::DockSurface::Scratchpad),
            "restoring the dock does not close the surface"
        );

        // The editor keeps its keys; only the ⤢ click restores it.
        app.state.dock_maximized = true;
        app.state.dock_tab = Some(crate::app::DockSurface::Editor);
        app.state.dock_editor_focused = true;
        assert!(
            !app.handle_dock_chooser_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
        );
        assert!(app.state.dock_maximized);
    }

    #[test]
    fn diff_whitespace_key_updates_session_state_and_invalidates_the_active_projection() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Diff);
        app.state.dock_diff_focused = true;
        app.state.dock_diff_active_key = Some(crate::app::state::DiffCacheKey {
            root: std::path::PathBuf::from("/repo"),
            base: "main".into(),
            ignore_whitespace: false,
        });

        assert!(
            app.handle_dock_diff_key(&TerminalKey::new(KeyCode::Char('w'), KeyModifiers::empty()))
        );
        assert!(app.state.dock_diff_ignore_whitespace);
        assert!(app.state.dock_diff_active_key.is_none());

        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.dock_tab = Some(crate::app::DockSurface::Diff);
        assert!(app.state.dock_diff_ignore_whitespace);
    }

    #[test]
    fn dock_hosted_pr_keys_open_checkout_and_shared_action_menus() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Pr);
        app.state.dock_pr_focused = true;

        assert!(
            app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Char('c'), KeyModifiers::empty()))
        );
        assert_eq!(
            app.state.dock_pr_checkout_menu,
            Some(crate::app::state::PrCheckoutChoice::CurrentCheckout)
        );
        assert!(app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Down, KeyModifiers::empty())));
        assert_eq!(
            app.state.dock_pr_checkout_menu,
            Some(crate::app::state::PrCheckoutChoice::NewWorktree)
        );
        assert!(app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.state.dock_pr_checkout_menu.is_none());

        assert!(
            app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
        );
        assert_eq!(
            app.state.dock_pr_action_menu,
            Some(crate::app::state::PrActionMenuState::default())
        );
        assert!(app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.state.dock_pr_action_menu.is_none());

        app.state.dock_pr_focused = false;
        assert!(
            !app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Char('c'), KeyModifiers::empty()))
        );
    }

    fn detail_with_comments(bodies: &[&str]) -> crate::work_index::WorkItemDetail {
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.comments = bodies
            .iter()
            .map(|body| crate::work_index::WorkItemComment {
                author: Some("ada".into()),
                body: (*body).into(),
                created_at: None,
            })
            .collect();
        detail
    }

    fn expanded_bodies(app: &App, key: &crate::app::state::WorkItemKey) -> Vec<String> {
        let Some(view) = app.state.dock_object_views.get(key) else {
            return Vec::new();
        };
        app.state
            .work_item_detail_cache
            .get(key)
            .map(|detail| {
                detail
                    .comments
                    .iter()
                    .filter(|comment| {
                        view.comment_is_expanded(crate::ui::work_list_detail::comment_identity(
                            comment,
                        ))
                    })
                    .map(|comment| comment.body.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_digit_expands_the_ticket_comment_it_names() {
        let mut app = dock_linear_test_app();
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        app.state
            .work_item_detail_cache
            .insert(key.clone(), detail_with_comments(&["first", "second"]));
        let press = |app: &mut App, code| {
            app.handle_dock_linear_key(&TerminalKey::new(code, KeyModifiers::empty()))
        };

        assert!(press(&mut app, KeyCode::Char('2')));
        assert_eq!(expanded_bodies(&app, &key), vec!["second".to_string()]);

        assert!(press(&mut app, KeyCode::Char('2')));
        assert!(
            expanded_bodies(&app, &key).is_empty(),
            "the same digit collapses it again"
        );
    }

    #[test]
    fn a_expands_every_ticket_comment_then_collapses_them_all() {
        let mut app = dock_linear_test_app();
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        app.state.work_item_detail_cache.insert(
            key.clone(),
            detail_with_comments(&["first", "second", "third"]),
        );
        let press = |app: &mut App, code| {
            app.handle_dock_linear_key(&TerminalKey::new(code, KeyModifiers::empty()))
        };

        assert!(press(&mut app, KeyCode::Char('a')));
        assert_eq!(expanded_bodies(&app, &key).len(), 3);

        assert!(press(&mut app, KeyCode::Char('a')));
        assert!(expanded_bodies(&app, &key).is_empty());

        // Expand-all after a single comment is already open still opens the
        // rest rather than reading as "all expanded" and collapsing.
        assert!(press(&mut app, KeyCode::Char('1')));
        assert!(press(&mut app, KeyCode::Char('a')));
        assert_eq!(expanded_bodies(&app, &key).len(), 3);
    }

    #[test]
    fn expanding_survives_a_comment_arriving_above_it() {
        let mut app = dock_linear_test_app();
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        app.state
            .work_item_detail_cache
            .insert(key.clone(), detail_with_comments(&["first", "second"]));

        assert!(app
            .handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::empty())));
        assert_eq!(expanded_bodies(&app, &key), vec!["first".to_string()]);

        // A refresh puts a newer comment at the head. Position moved; identity
        // did not, so the reader keeps the comment they opened.
        app.state.work_item_detail_cache.insert(
            key.clone(),
            detail_with_comments(&["newest", "first", "second"]),
        );

        assert_eq!(expanded_bodies(&app, &key), vec!["first".to_string()]);
    }

    #[test]
    fn a_comment_key_with_nothing_to_toggle_still_reaches_the_pane() {
        let mut app = dock_linear_test_app();
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        app.state
            .work_item_detail_cache
            .insert(key.clone(), detail_with_comments(&["only one"]));
        let press = |app: &mut App, code| {
            app.handle_dock_linear_key(&TerminalKey::new(code, KeyModifiers::empty()))
        };

        assert!(
            press(&mut app, KeyCode::Char('1')),
            "the one comment toggles"
        );
        assert!(
            !press(&mut app, KeyCode::Char('2')),
            "a digit past the last comment is not the dock's key"
        );

        app.state
            .work_item_detail_cache
            .insert(key, crate::work_index::WorkItemDetail::empty());
        assert!(
            !press(&mut app, KeyCode::Char('1')),
            "no comments, so the digit belongs to the pane"
        );
        assert!(!press(&mut app, KeyCode::Char('a')));
    }

    #[test]
    fn pr_comment_digits_only_bind_on_the_sub_tab_that_shows_comments() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Pr);
        app.state.dock_pr_focused = true;
        // No pull request is focused, so the digit has no subject either way;
        // what matters is that a non-overview sub-tab leaves the key alone.
        assert!(crate::ui::dock::pr::focused_pr_key(&app.state).is_none());
        assert!(
            !app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::empty()))
        );
        assert!(
            !app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Char('a'), KeyModifiers::empty()))
        );
    }

    fn dock_linear_test_app() -> App {
        let mut app = test_app();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Linear);
        app.state.dock_linear_focused = true;
        app.state.work_index_session.linear.viewer = Some("Viewer Name".into());
        app.state
            .work_index_session
            .set_linear_viewer_identity_for_test("viewer-id");
        app
    }

    fn dock_ticket_sections(app: &App) -> Vec<crate::app::state::DetailSection> {
        let area = dock_detail_area(&app.state);
        crate::ui::dock::linear::focused_ticket_layout(&app.state, area)
            .map(|layout| {
                layout
                    .sections
                    .iter()
                    .map(|(_, section)| *section)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn collapsed_sections(
        app: &App,
        key: &crate::app::state::WorkItemKey,
    ) -> Vec<crate::app::state::DetailSection> {
        let Some(view) = app.state.dock_object_views.get(key) else {
            return Vec::new();
        };
        dock_ticket_sections(app)
            .into_iter()
            .filter(|section| view.section_is_collapsed(*section))
            .collect()
    }

    #[test]
    fn an_alt_digit_folds_the_section_the_detail_numbers() {
        let mut app = dock_linear_test_app();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_body_rect = ratatui::layout::Rect::new(0, 20, 100, 20);
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        let sections = dock_ticket_sections(&app);
        assert!(sections.len() >= 2, "{sections:?}");
        let second = sections[1];
        let press = |app: &mut App, code| {
            app.handle_dock_linear_key(&TerminalKey::new(code, KeyModifiers::ALT))
        };

        assert!(press(&mut app, KeyCode::Char('2')));
        assert_eq!(collapsed_sections(&app, &key), vec![second]);

        assert!(press(&mut app, KeyCode::Char('2')));
        assert!(
            collapsed_sections(&app, &key).is_empty(),
            "the same digit opens it again"
        );

        // Past the last section there is nothing to fold, so the key is not
        // the dock's and still reaches the pane.
        let past = char::from_digit(sections.len() as u32 + 1, 10).expect("a digit past the last");
        assert!(!press(&mut app, KeyCode::Char(past)));
    }

    #[test]
    fn clicking_a_section_header_folds_it_and_a_body_row_only_focuses() {
        let mut app = dock_linear_test_app();
        let area = ratatui::layout::Rect::new(0, 20, 100, 20);
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_rect = area;
        app.state.view.dock_body_rect = area;
        app.state.dock_linear_focused = false;
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        let layout = crate::ui::dock::linear::focused_ticket_layout(&app.state, area)
            .expect("a ticket detail layout");
        let (header, section) = layout.sections[0];
        let header_row = area.y + u16::try_from(header).expect("a header on screen");

        let click = |app: &mut App, row| {
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: area.x + 2,
                row,
                modifiers: KeyModifiers::empty(),
            })
        };

        click(&mut app, header_row);
        assert_eq!(collapsed_sections(&app, &key), vec![section]);
        assert!(
            app.state.dock_linear_focused,
            "folding a header also takes focus, like every other dock click"
        );

        // The blank line above the rule is not the header, so it falls through
        // to the plain focus behaviour and folds nothing.
        click(&mut app, header_row - 1);
        assert_eq!(collapsed_sections(&app, &key), vec![section]);
    }

    #[test]
    fn clicking_the_ticket_buttons_opens_the_menus_a_click_can_then_pick_from() {
        let mut app = dock_linear_test_app();
        let area = ratatui::layout::Rect::new(0, 20, 100, 20);
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_rect = area;
        app.state.view.dock_body_rect = area;
        app.work_index_linearis_program_override = Some(std::path::PathBuf::from("/usr/bin/false"));
        let click = |app: &mut App, column, row| {
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::empty(),
            })
        };
        // Both controls share the action row under the heading.
        let action_row = area.y + 1;
        let more_column = area.x
            + 1
            + u16::try_from(
                crate::ui::text::display_width(crate::ui::work_view::TICKET_START_LABEL) + 1,
            )
            .expect("a narrow label");

        click(&mut app, area.x + 3, action_row);
        assert!(
            app.state.dock_ticket_start_menu.is_some(),
            "clicking [Start thread] opens its menu"
        );

        // A click outside an open menu dismisses it, exactly as Esc does.
        click(&mut app, area.x + 3, area.bottom() - 1);
        assert!(app.state.dock_ticket_start_menu.is_none());

        click(&mut app, more_column, action_row);
        assert!(
            app.state.dock_ticket_action_menu.is_some(),
            "clicking [⋯] opens the action menu"
        );

        // The transition submenu is the first entry, so picking it by click
        // proves a menu row activates rather than only moving a cursor.
        let menu_area = dock_detail_area(&app.state);
        let layout = crate::ui::dock::linear::focused_ticket_layout(&app.state, menu_area)
            .expect("a ticket detail layout");
        let anchor = crate::ui::work_view::ticket_action_menu_anchor(menu_area, layout.action_rows);
        let context = app
            .dock_ticket_action_context()
            .expect("a focused ticket context");
        let menu = crate::ui::ticket_actions::ticket_action_menu_layout(
            anchor,
            menu_area,
            &context,
            app.state.dock_ticket_action_menu.expect("an open menu"),
        )
        .expect("a menu layout");
        let transition_row = crate::ui::ticket_actions::ticket_action_table(
            &context,
            crate::ui::ticket_actions::TicketActionMenuPage::Actions,
        )
        .iter()
        .position(|entry| entry.action == crate::ui::ticket_actions::TicketAction::TransitionMenu)
        .expect("a transition entry");
        click(
            &mut app,
            menu.list_rect.x + 1,
            menu.list_rect.y + u16::try_from(transition_row).expect("a visible row"),
        );
        assert_eq!(
            app.state.dock_ticket_action_menu.map(|menu| menu.page),
            Some(crate::ui::ticket_actions::TicketActionMenuPage::Transitions),
            "clicking an entry runs it instead of only moving the cursor"
        );
    }

    #[test]
    fn a_ticket_button_click_never_overtakes_a_draft_or_a_pending_write() {
        let mut app = dock_linear_test_app();
        let area = ratatui::layout::Rect::new(0, 20, 100, 20);
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_rect = area;
        app.state.view.dock_body_rect = area;
        let click = |app: &mut App| {
            app.handle_mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: area.x + 3,
                row: area.y + 1,
                modifiers: KeyModifiers::empty(),
            })
        };

        app.state.dock_ticket_comment_draft = Some("half-written".into());
        click(&mut app);
        assert_eq!(
            app.state.dock_ticket_comment_draft.as_deref(),
            Some("half-written"),
            "a click must not reopen the menu over a typed draft"
        );
        assert!(app.state.dock_ticket_start_menu.is_none());

        app.state.dock_ticket_comment_draft = None;
        app.state.dock_pending_write = Some(crate::work_index::WorkItemWrite::TransitionTicket {
            identifier: "SCA-1".into(),
            state: "Done".into(),
        });
        click(&mut app);
        assert!(
            app.state.dock_ticket_start_menu.is_none(),
            "a pending confirmation owns the surface until it is answered"
        );
    }

    #[test]
    fn a_section_click_follows_the_scrolled_line_it_lands_on() {
        let mut app = dock_linear_test_app();
        let area = ratatui::layout::Rect::new(0, 20, 100, 6);
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_rect = area;
        app.state.view.dock_body_rect = area;
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        let layout = crate::ui::dock::linear::focused_ticket_layout(&app.state, area)
            .expect("a ticket detail layout");
        let (header, section) = *layout
            .sections
            .iter()
            .find(|(header, _)| *header >= usize::from(area.height))
            .expect("a section below the first screen");
        // Scroll it into view; the click row is then measured from the scroll,
        // not from the top of the content.
        let scroll = u16::try_from(header).expect("a scrollable header");
        app.state
            .dock_object_views
            .entry(key.clone())
            .or_default()
            .scroll = scroll;
        let max_scroll = layout.lines.len().saturating_sub(usize::from(area.height));
        let clamped = usize::from(scroll).min(max_scroll);

        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x + 2,
            row: area.y + u16::try_from(header - clamped).expect("a visible row"),
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(collapsed_sections(&app, &key), vec![section]);
    }

    #[test]
    fn folding_comments_away_gives_their_digits_back_to_the_pane() {
        let mut app = dock_linear_test_app();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_rect = ratatui::layout::Rect::new(0, 20, 100, 20);
        app.state.view.dock_body_rect = app.state.view.dock_rect;
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        app.state
            .work_item_detail_cache
            .insert(key.clone(), detail_with_comments(&["first", "second"]));
        let press = |app: &mut App, code, modifiers| {
            app.handle_dock_linear_key(&TerminalKey::new(code, modifiers))
        };

        assert!(press(&mut app, KeyCode::Char('1'), KeyModifiers::empty()));
        assert_eq!(expanded_bodies(&app, &key), vec!["first".to_string()]);

        // Fold the comment section itself. Its digits address the list by
        // position, so with the list off screen they are the pane's again.
        let comments = dock_ticket_sections(&app)
            .iter()
            .position(|section| *section == crate::app::state::DetailSection::Comments)
            .expect("a comments section");
        let digit = char::from_digit(comments as u32 + 1, 10).expect("a section digit");
        assert!(press(&mut app, KeyCode::Char(digit), KeyModifiers::ALT));

        assert!(!press(&mut app, KeyCode::Char('2'), KeyModifiers::empty()));
        assert!(!press(&mut app, KeyCode::Char('a'), KeyModifiers::empty()));
        assert_eq!(
            expanded_bodies(&app, &key),
            vec!["first".to_string()],
            "and the expansion the reader already made is untouched"
        );
    }

    #[test]
    fn the_pull_request_surface_folds_on_the_same_alt_digits() {
        let mut app = test_app();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Pr);
        app.state.dock_pr_focused = true;
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_rect = ratatui::layout::Rect::new(0, 20, 100, 20);
        app.state.view.dock_body_rect = app.state.view.dock_rect;
        // The sidebar fixture has no pull request, so bind one to the focused
        // pane and put the matching item in the index the dock reads.
        let url = "https://github.com/owner/repo/pull/7";
        for terminal in app.state.terminals.values_mut() {
            terminal.replace_prevalidated_manual_work_context(
                crate::work_context::PaneWorkContext {
                    pr_urls: vec![url.into()],
                    repo: Some("owner/repo".into()),
                    ..Default::default()
                },
            );
        }
        if let Some(snapshot) = app.state.work_index_snapshot.as_mut() {
            snapshot.items.push(crate::work_index::WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(7),
                pr_url: Some(url.into()),
                pr_title: Some("a pull request".into()),
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
                audience: crate::work_index::PrAudience::Other,
                cached_pr_detail: None,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: None,
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: Default::default(),
            });
        }
        let key = crate::ui::dock::pr::focused_pr_key(&app.state).expect("a focused pull request");
        let area = dock_detail_area(&app.state);
        let layout = crate::ui::dock::pr::focused_pr_layout(&app.state, area)
            .expect("a pull-request detail layout");
        let (_, first) = layout.sections.first().copied().expect("a first section");

        assert!(app.handle_dock_pr_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::ALT)));
        assert!(app
            .state
            .dock_object_views
            .get(&key)
            .is_some_and(|view| view.section_is_collapsed(first)));
    }

    #[test]
    fn a_modal_keeps_the_alt_digit_off_the_detail_and_off_the_pane() {
        let mut app = dock_linear_test_app();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.dock_rect = ratatui::layout::Rect::new(0, 20, 100, 20);
        app.state.view.dock_body_rect = app.state.view.dock_rect;
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        app.state.dock_ticket_comment_draft = Some("half a comment".into());

        assert!(
            app.handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::ALT)),
            "the draft owns the keyboard, so the key must not reach the pane"
        );
        assert!(
            collapsed_sections(&app, &key).is_empty(),
            "and it must not fold a section behind the draft either"
        );

        app.state.dock_ticket_comment_draft = None;
        assert!(
            app.handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::ALT))
        );
        assert_eq!(collapsed_sections(&app, &key).len(), 1);
    }

    #[test]
    fn a_collapsed_dock_preview_folds_on_click_like_the_dock_tab_does() {
        let mut app = dock_linear_test_app();
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        app.state.view.terminal_area = area;
        app.state.dock_collapsed = true;
        app.state.dock_tab = None;
        let key =
            crate::ui::dock::linear::focused_ticket_key(&app.state).expect("a focused ticket");
        app.state.dock_object_preview = Some(crate::app::state::DockObjectRef {
            surface: crate::app::DockSurface::Linear,
            key: key.ticket_id.clone().unwrap_or_default(),
        });
        let layout = crate::ui::dock::linear::focused_ticket_layout(&app.state, area)
            .expect("a ticket detail layout");
        let (header, section) = layout.sections[0];

        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x + 2,
            row: area.y + u16::try_from(header).expect("a header on screen"),
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(collapsed_sections(&app, &key), vec![section]);
    }

    #[test]
    fn a_digit_attaches_a_ticket_when_the_linear_surface_has_none() {
        let mut app = dock_linear_test_app();
        let terminal_id = app.state.workspaces[0].tabs[0]
            .panes
            .values()
            .next()
            .map(|pane| pane.attached_terminal_id.clone())
            .expect("a pane terminal");
        // Focus a pane whose context resolves no ticket, so the surface shows
        // the picker instead of a detail.
        for terminal in app.state.terminals.values_mut() {
            terminal.replace_prevalidated_manual_work_context(
                crate::work_context::PaneWorkContext::default(),
            );
        }
        assert!(crate::ui::dock::linear::focused_ticket_key(&app.state).is_none());
        let first = crate::ui::dock::linear::attachable_ticket_for_digit(&app.state, '1')
            .expect("an indexed ticket to attach");

        assert!(app
            .handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::empty())));
        let focused_terminal = app.state.workspaces[0]
            .focused_pane_id()
            .and_then(|pane_id| app.state.workspaces[0].terminal_id(pane_id))
            .cloned()
            .unwrap_or(terminal_id);
        assert_eq!(
            app.state.terminals[&focused_terminal]
                .work_context
                .effective()
                .primary_ticket(),
            Some(first.as_str())
        );
    }

    #[test]
    fn attaching_replaces_the_picker_tab_instead_of_adding_a_second_one() {
        let mut app = dock_linear_test_app();
        for terminal in app.state.terminals.values_mut() {
            terminal.replace_prevalidated_manual_work_context(
                crate::work_context::PaneWorkContext::default(),
            );
        }
        app.state.reconcile_dock_context_tabs();
        app.state
            .activate_dock_surface(crate::app::DockSurface::Linear);

        assert!(app
            .handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::empty())));

        let linear_tabs = app
            .state
            .dock_open_surfaces
            .iter()
            .filter(|surface| **surface == crate::app::DockSurface::Linear)
            .count();
        assert_eq!(linear_tabs, 1, "{:?}", app.state.dock_open_surfaces);
        assert!(crate::ui::dock::linear::focused_ticket_item(&app.state).is_some());
    }

    #[test]
    fn attaching_drops_a_draft_staged_against_the_previous_subject() {
        let mut app = dock_linear_test_app();
        for terminal in app.state.terminals.values_mut() {
            terminal.replace_prevalidated_manual_work_context(
                crate::work_context::PaneWorkContext::default(),
            );
        }
        app.state.reconcile_dock_context_tabs();
        app.state
            .activate_dock_surface(crate::app::DockSurface::Linear);
        app.state.dock_ticket_comment_draft = Some("half-written".into());
        app.state.dock_ticket_action_menu = Some(Default::default());

        assert!(app
            .handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('1'), KeyModifiers::empty())));

        assert!(app.state.dock_ticket_comment_draft.is_none());
        assert!(app.state.dock_ticket_action_menu.is_none());
    }

    #[test]
    fn the_picker_swallows_digits_but_not_other_keys() {
        let mut app = dock_linear_test_app();
        for terminal in app.state.terminals.values_mut() {
            terminal.replace_prevalidated_manual_work_context(
                crate::work_context::PaneWorkContext::default(),
            );
        }
        assert!(!app
            .handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty())));
        assert!(app
            .handle_dock_linear_key(&TerminalKey::new(KeyCode::Char('9'), KeyModifiers::empty())));
    }

    #[test]
    fn dock_hosted_linear_menu_stages_viewer_and_priority_writes() {
        let mut app = dock_linear_test_app();
        app.work_index_linearis_program_override = Some(std::path::PathBuf::from("/usr/bin/false"));
        let key = |code| TerminalKey::new(code, KeyModifiers::empty());

        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Char('m'))));
        for _ in 0..5 {
            assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Down)));
        }
        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Enter)));
        assert_eq!(
            app.state.dock_pending_write,
            Some(crate::work_index::WorkItemWrite::AssignTicket {
                identifier: "SCA-3102".into(),
                assignee: "viewer-id".into(),
            })
        );
        assert!(app.state.dock_write_notice.is_none());
        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Esc)));

        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Char('m'))));
        for _ in 0..6 {
            assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Down)));
        }
        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Enter)));
        assert_eq!(
            app.state.dock_ticket_action_menu.map(|menu| menu.page),
            Some(crate::ui::ticket_actions::TicketActionMenuPage::Priorities)
        );
        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Down)));
        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Enter)));
        assert_eq!(
            app.state.dock_pending_write,
            Some(crate::work_index::WorkItemWrite::SetTicketPriority {
                identifier: "SCA-3102".into(),
                priority: 1,
            })
        );
    }

    #[test]
    fn dock_hosted_linear_question_opens_ticket_home_with_fixed_prefix() {
        let mut app = dock_linear_test_app();
        let key = |code| TerminalKey::new(code, KeyModifiers::empty());

        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Char('m'))));
        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Down)));
        assert!(app.handle_dock_linear_key_headless(&key(KeyCode::Enter)));

        let home = app.state.home.as_ref().expect("ticket home card");
        assert_eq!(home.prompt, "About SCA-3102 (annual credits): ");
        assert_eq!(
            home.ticket
                .as_ref()
                .map(|ticket| ticket.identifier.as_str()),
            Some("SCA-3102")
        );
    }

    #[test]
    fn full_ticket_menu_keyboard_uses_shared_assignment_action() {
        let mut app = dock_linear_test_app();
        let mut view =
            crate::app::state::WorkViewState::new(true, app.state.work_index_snapshot.clone());
        view.projection = crate::app::state::WorkProjection::Tickets;
        view.selected = Some(crate::app::state::WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some("SCA-3102".into()),
        });
        app.state.work_view = Some(view);

        assert!(app.handle_work_view_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::empty(),)));
        for _ in 0..5 {
            assert!(app.handle_work_view_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty(),)));
        }
        assert!(app.handle_work_view_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .and_then(|view| view.pending_write.as_ref()),
            Some(&crate::work_index::WorkItemWrite::AssignTicket {
                identifier: "SCA-3102".into(),
                assignee: "viewer-id".into(),
            })
        );
    }

    #[test]
    fn full_ticket_explain_uses_fixed_prompt_and_cancel_stages_confirmation() {
        let mut app = dock_linear_test_app();
        let mut view =
            crate::app::state::WorkViewState::new(true, app.state.work_index_snapshot.clone());
        view.projection = crate::app::state::WorkProjection::Tickets;
        view.selected = Some(crate::app::state::WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some("SCA-3102".into()),
        });
        app.state.work_view = Some(view.clone());
        let key = |code| KeyEvent::new(code, KeyModifiers::empty());

        assert!(app.handle_work_view_key(key(KeyCode::Char('m'))));
        assert!(app.handle_work_view_key(key(KeyCode::Down)));
        assert!(app.handle_work_view_key(key(KeyCode::Down)));
        assert!(app.handle_work_view_key(key(KeyCode::Enter)));
        assert_eq!(
            app.state.home.as_ref().map(|home| home.prompt.as_str()),
            Some(
                "Summarise SCA-3102 (annual credits). Include the ticket, linked pull requests, and the acceptance criteria block."
            )
        );

        app.state.home = None;
        app.state.work_view = Some(view);
        assert!(app.handle_work_view_key(key(KeyCode::Char('m'))));
        for _ in 0..12 {
            assert!(app.handle_work_view_key(key(KeyCode::Down)));
        }
        assert!(app.handle_work_view_key(key(KeyCode::Enter)));
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .and_then(|view| view.pending_write.as_ref()),
            Some(&crate::work_index::WorkItemWrite::TransitionTicket {
                identifier: "SCA-3102".into(),
                state: "Canceled".into(),
            })
        );
    }

    #[test]
    fn shared_pr_confirmation_cancels_or_dispatches_the_selected_merge_method() {
        let mut app = test_app();
        let key = crate::app::state::WorkItemKey {
            repo: "owner/repo".into(),
            pr_number: Some(42),
            pr_url: Some("https://github.com/owner/repo/pull/42".into()),
            ticket_id: None,
        };
        let confirmation = crate::app::state::PrActionConfirmation {
            key,
            action: crate::ui::work_list_detail::PrActionKind::Merge(
                crate::config::MergeMethodConfig::Rebase,
            ),
        };
        app.state.pr_action_confirmation = Some(confirmation.clone());
        assert!(app
            .handle_pr_action_confirmation_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.state.pr_action_confirmation.is_none());
        assert!(app.state.request_pr_command.is_none());

        app.state.pr_action_confirmation = Some(confirmation);
        assert!(app.handle_pr_action_confirmation_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty()
        )));
        assert!(matches!(
            app.state.request_pr_command,
            Some(crate::app::state::PrCommandRequest {
                action: crate::app::state::PrCommandAction::Merge(
                    crate::config::MergeMethodConfig::Rebase
                ),
                ..
            })
        ));
    }

    #[test]
    fn pr_action_confirmation_is_isolated_between_clients() {
        let mut app = test_app();
        let mut client_a = crate::app::state::DockPresentationState::default();
        let mut client_b = crate::app::state::DockPresentationState::default();
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

        app.state.swap_dock_presentation(&mut client_a);
        app.state.pr_action_confirmation = Some(confirmation.clone());
        app.state.swap_dock_presentation(&mut client_a);

        app.state.swap_dock_presentation(&mut client_b);
        assert!(!app.handle_pr_action_confirmation_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty()
        )));
        assert!(app.state.request_pr_command.is_none());
        app.state.swap_dock_presentation(&mut client_b);

        app.state.swap_dock_presentation(&mut client_a);
        assert_eq!(app.state.pr_action_confirmation, Some(confirmation));
    }

    #[test]
    fn usage_view_passes_tab_switch_keybinds_through_and_closes() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        app.state.workspaces[0].test_add_tab(None);
        app.state.workspaces[0].active_tab = 0;
        app.state.active = Some(0);
        app.state.keybinds.next_tab = crate::config::ActionKeybinds::direct("alt+n");
        app.toggle_usage_view();

        assert!(app.handle_usage_view_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT)));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert!(app.state.usage_view.is_none());
    }

    #[test]
    fn usage_view_prefix_key_enters_prefix_mode() {
        let mut app = test_app();
        app.toggle_usage_view();
        let (code, modifiers) = (app.state.prefix_code, app.state.prefix_mods);
        app.handle_usage_view_key(KeyEvent::new(code, modifiers));
        assert_eq!(app.state.server_mode(), Mode::Prefix);
    }

    #[test]
    fn usage_view_keys_change_metric_range_breakdown_and_close() {
        use crate::app::state::{UsageBreakdown, UsageMetric, UsageRange};

        let mut app = test_app();
        app.toggle_usage_view();
        assert!(app.state.usage_view.is_some());
        for (key, metric, range, breakdown) in [
            (
                't',
                UsageMetric::Tokens,
                UsageRange::Days30,
                UsageBreakdown::Model,
            ),
            (
                '1',
                UsageMetric::Tokens,
                UsageRange::Hours24,
                UsageBreakdown::Model,
            ),
            (
                '7',
                UsageMetric::Tokens,
                UsageRange::Days7,
                UsageBreakdown::Model,
            ),
            (
                '9',
                UsageMetric::Tokens,
                UsageRange::Days90,
                UsageBreakdown::Model,
            ),
            (
                '3',
                UsageMetric::Tokens,
                UsageRange::Days30,
                UsageBreakdown::Model,
            ),
            (
                'd',
                UsageMetric::Tokens,
                UsageRange::Days30,
                UsageBreakdown::Day,
            ),
            (
                'm',
                UsageMetric::Tokens,
                UsageRange::Days30,
                UsageBreakdown::Model,
            ),
            (
                'c',
                UsageMetric::Cost,
                UsageRange::Days30,
                UsageBreakdown::Model,
            ),
        ] {
            assert!(app
                .handle_usage_view_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::empty(),)));
            let view = app.state.usage_view.as_ref().expect("usage view");
            assert_eq!(
                (view.metric, view.range, view.breakdown),
                (metric, range, breakdown)
            );
        }
        assert!(app.handle_usage_view_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.state.usage_view.is_none());
    }

    #[test]
    fn usage_view_opens_from_cached_snapshot_while_rescan_runs() {
        let mut app = test_app();
        app.state.usage_snapshot = Some(crate::provider_usage::UsageSnapshot::default());
        app.toggle_usage_view();
        let view = app.state.usage_view.as_ref().expect("usage view");
        assert!(view.snapshot.is_some());
        assert!(view.scanning, "cached rows remain visible during rescan");
        assert_eq!(app.usage_scan_in_flight, Some(1));
    }

    #[test]
    fn usage_scan_ignores_stale_generation_and_applies_current_result() {
        let mut app = test_app();
        app.toggle_usage_view();
        app.start_usage_scan();
        assert_eq!(app.usage_scan_in_flight, Some(2));
        assert!(!app.handle_usage_scan_finished(
            1,
            Ok(Box::new(crate::provider_usage::UsageSnapshot::default())),
        ));
        assert!(app
            .state
            .usage_view
            .as_ref()
            .is_some_and(|view| view.scanning));
        assert!(app.handle_usage_scan_finished(
            2,
            Ok(Box::new(crate::provider_usage::UsageSnapshot::default())),
        ));
        assert!(app
            .state
            .usage_view
            .as_ref()
            .is_some_and(|view| !view.scanning && view.snapshot.is_some()));
    }

    #[test]
    fn the_surface_menu_handles_navigation_and_selection() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_surface_menu = Some(crate::app::state::DockSurfaceMenu { selected: 0 });

        assert!(app
            .handle_dock_surface_menu_key(&TerminalKey::new(KeyCode::Down, KeyModifiers::empty())));
        assert_eq!(
            app.state.dock_surface_menu.map(|menu| menu.selected),
            Some(1)
        );
        assert!(app.handle_dock_surface_menu_key(&TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty()
        )));
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Files));
        assert!(app.state.dock_surface_menu.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agents_keys_move_and_open_a_known_transcript_in_the_editor() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("claude")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.set_server_mode(Mode::Terminal);
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let pane_terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("pane terminal")
            .clone();
        let second_path = std::path::PathBuf::from("/tmp/second.jsonl");
        let terminal = app
            .state
            .terminals
            .get_mut(&pane_terminal_id)
            .expect("terminal state");
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        terminal.claude_transcript_session_id = Some("session".into());
        terminal.claude_subagent_observations = Some(vec![
            crate::app::claude_subagents::ClaudeSubagentObservation {
                id: "first".into(),
                parent_id: None,
                name: "first".into(),
                state: crate::detect::AgentState::Working,
                observed_at: None,
                transcript_path: None,
            },
            crate::app::claude_subagents::ClaudeSubagentObservation {
                id: "second".into(),
                parent_id: None,
                name: "second".into(),
                state: crate::detect::AgentState::Idle,
                observed_at: None,
                transcript_path: Some(second_path),
            },
        ]);
        let editor_terminal_id = crate::terminal::TerminalId::alloc();
        let (runtime, mut input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(editor_terminal_id.clone(), runtime);
        app.state.dock_editor_sessions.insert(
            pane_id,
            crate::app::state::DockEditorSession {
                pane_id: crate::layout::PaneId::alloc(),
                terminal_id: editor_terminal_id,
            },
        );
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Agents);
        app.state.dock_agents_focused = true;
        app.state.dock_agents_selection = Some("first".into());

        assert!(app.handle_dock_agents_key_headless(&TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty()
        )));
        assert!(input.try_recv().is_err());
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Agents));
        assert!(app.handle_dock_agents_key_headless(&TerminalKey::new(
            KeyCode::Down,
            KeyModifiers::empty()
        )));
        assert_eq!(app.state.dock_agents_selection.as_deref(), Some("second"));
        assert!(app.handle_dock_agents_key_headless(&TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty()
        )));

        assert_eq!(
            input.try_recv().expect("editor input"),
            Bytes::from_static(b"\x1b:e /tmp/second.jsonl\r")
        );
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Editor));
    }

    fn files_with_open_surface_menu() -> App {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Files);
        app.state.dock_open_surfaces = vec![crate::app::DockSurface::Files];
        app.state.dock_files_focused = true;
        app.state.dock_files_filter = "needle".to_string();
        app.state.dock_surface_menu = Some(crate::app::state::DockSurfaceMenu { selected: 0 });
        app
    }

    #[tokio::test]
    async fn open_surface_menu_swallows_files_input_and_handles_terminal_shortcut() {
        let mut app = files_with_open_surface_menu();

        assert!(app
            .handle_key(TerminalKey::new(KeyCode::Char('z'), KeyModifiers::empty()))
            .await
            .is_none());
        assert_eq!(app.state.dock_files_filter, "needle");
        assert!(app.state.dock_surface_menu.is_some());

        assert!(app
            .handle_key(TerminalKey::new(KeyCode::Char('t'), KeyModifiers::empty()))
            .await
            .is_none());
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Terminal));
        assert_eq!(app.state.dock_files_filter, "needle");
        assert!(app.state.dock_surface_menu.is_none());
    }

    #[tokio::test]
    async fn surface_menu_escape_restores_files_focus() {
        let mut app = files_with_open_surface_menu();

        assert!(app
            .handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await
            .is_none());

        assert!(app.state.dock_surface_menu.is_none());
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Files));
        assert!(app.state.dock_files_focused);
        assert_eq!(app.state.dock_files_filter, "needle");
    }

    #[tokio::test]
    async fn key_dismisses_a_visible_hover_tooltip() {
        let mut app = app_for_mouse_test();
        app.state.hovered_control = Some(crate::app::state::ControlId::SidebarMore);
        app.state.hover_tooltip_visible = true;

        let _ = app
            .handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.hovered_control, None);
        assert!(!app.state.hover_tooltip_visible);
    }

    #[test]
    fn selected_pr_checkout_carries_context_into_home_plan() {
        let mut app = test_app();
        let item = crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: Some(42),
            pr_url: Some("https://github.com/owner/repo/pull/42".into()),
            pr_title: Some("repair parser".into()),
            pr_state: Some("open".into()),
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 1,
            deletions: 0,
            author: Some("ada".into()),
            assignees: vec!["ada".into()],
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Passing,
            audience: crate::work_index::PrAudience::Authored,
            cached_pr_detail: None,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: Some("fix/parser".into()),
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: Default::default(),
        };
        app.state.work_view = Some(crate::app::state::WorkViewState::new(
            true,
            Some(crate::work_index::Snapshot {
                items: vec![item],
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: std::time::SystemTime::now(),
            }),
        ));

        app.open_selected_pr_checkout();

        let home = app.state.home.as_mut().expect("checkout should open home");
        assert_eq!(
            home.pr,
            Some(crate::app::home::HomePrContext {
                url: "https://github.com/owner/repo/pull/42".into(),
                number: 42,
                repo: "owner/repo".into(),
            })
        );
        home.prompt = "continue the review".into();
        assert_eq!(home.dispatch_plan().expect("dispatch plan").pr, home.pr);
    }

    #[test]
    fn cannot_activate_ask_question_without_pr_head_context() {
        let mut app = test_app();
        let item = crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: Some(7),
            pr_url: Some("https://github.com/owner/repo/pull/7".into()),
            pr_title: Some("repair parser".into()),
            pr_state: Some("open".into()),
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 1,
            deletions: 0,
            author: Some("ada".into()),
            assignees: vec!["ada".into()],
            labels: vec!["bug".into()],
            check_state: crate::work_index::PrCheckState::Passing,
            audience: crate::work_index::PrAudience::Authored,
            cached_pr_detail: None,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: Default::default(),
        };

        app.state.work_view = Some(crate::app::state::WorkViewState::new(
            true,
            Some(crate::work_index::Snapshot {
                items: vec![item],
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: std::time::SystemTime::now(),
            }),
        ));

        app.activate_pr_action(
            crate::app::state::WorkItemKey {
                repo: "owner/repo".into(),
                pr_number: Some(7),
                pr_url: Some("https://github.com/owner/repo/pull/7".into()),
                ticket_id: None,
            },
            crate::ui::work_list_detail::PrActionKind::AskQuestion,
        );
        assert!(app.state.home.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn fix_findings_fetches_only_unresolved_review_thread_comments() {
        use std::os::unix::fs::PermissionsExt;

        let (mut app, key) = pr_action_test_app();
        let fixture_dir =
            std::env::temp_dir().join(format!("herdr-fix-findings-{}", std::process::id()));
        std::fs::create_dir_all(&fixture_dir).expect("create fixture directory");
        let gh = fixture_dir.join("gh");
        std::fs::write(
            &gh,
            r#"#!/bin/sh
printf '%s' '{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"isResolved":true,"comments":{"nodes":[{"path":"src/resolved.rs","line":4,"body":"resolved finding","author":{"login":"resolved-reviewer"}}]}},{"isResolved":false,"comments":{"nodes":[{"path":"src/parser.rs","line":17,"body":"handle the empty token","author":{"login":"reviewer"}}]}}]}}}}}'
"#,
        )
        .expect("write fake gh");
        let mut permissions = std::fs::metadata(&gh)
            .expect("fake gh metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).expect("make fake gh executable");
        app.work_index_gh_program_override = Some(gh);

        app.activate_pr_action(key, crate::ui::work_list_detail::PrActionKind::FixFindings);

        let prompt = &app.state.home.as_ref().expect("fix thread home").prompt;
        assert_eq!(
            prompt,
            "src/parser.rs:17 — reviewer: handle the empty token"
        );
        assert!(!prompt.contains("resolved finding"));
        assert!(!prompt.contains("ordinary issue comment"));
        let _ = std::fs::remove_dir_all(fixture_dir);
    }

    #[cfg(unix)]
    #[test]
    fn fix_findings_dims_after_fetch_finds_no_unresolved_threads() {
        use std::os::unix::fs::PermissionsExt;

        let (mut app, key) = pr_action_test_app();
        let fixture_dir =
            std::env::temp_dir().join(format!("herdr-fix-findings-empty-{}", std::process::id()));
        std::fs::create_dir_all(&fixture_dir).expect("create fixture directory");
        let gh = fixture_dir.join("gh");
        std::fs::write(
            &gh,
            r#"#!/bin/sh
printf '%s' '{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"isResolved":true,"comments":{"nodes":[{"path":"src/resolved.rs","line":4,"body":"done","author":{"login":"reviewer"}}]}}]}}}}}'
"#,
        )
        .expect("write fake gh");
        let mut permissions = std::fs::metadata(&gh)
            .expect("fake gh metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).expect("make fake gh executable");
        app.work_index_gh_program_override = Some(gh);

        app.activate_pr_action(
            key.clone(),
            crate::ui::work_list_detail::PrActionKind::FixFindings,
        );

        assert!(app.state.home.is_none());
        let fix_findings = app
            .pr_action_table(&key)
            .expect("PR action table")
            .into_iter()
            .find(|action| action.kind == crate::ui::work_list_detail::PrActionKind::FixFindings)
            .expect("fix findings action");
        assert_eq!(
            fix_findings.disabled_reason,
            Some("No unresolved review threads")
        );
        let _ = std::fs::remove_dir_all(fixture_dir);
    }

    #[test]
    fn selected_pr_action_menu_stays_interactive_without_head_context() {
        let mut app = test_app();
        let item = crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: Some(42),
            pr_url: Some("https://github.com/owner/repo/pull/42".into()),
            pr_title: Some("repair parser".into()),
            pr_state: Some("open".into()),
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 1,
            deletions: 0,
            author: Some("ada".into()),
            assignees: vec!["ada".into()],
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Passing,
            audience: crate::work_index::PrAudience::Authored,
            cached_pr_detail: None,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: Default::default(),
        };
        app.state.work_view = Some(crate::app::state::WorkViewState::new(
            true,
            Some(crate::work_index::Snapshot {
                items: vec![item],
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: std::time::SystemTime::now(),
            }),
        ));

        let actions = app.selected_pr_action_table();
        fn enabled(
            actions: &[crate::ui::work_list_detail::PrAction],
            kind: crate::ui::work_list_detail::PrActionKind,
        ) -> bool {
            actions
                .iter()
                .find(|action| action.kind == kind)
                .is_some_and(crate::ui::work_list_detail::PrAction::enabled)
        }

        assert!(enabled(
            &actions,
            crate::ui::work_list_detail::PrActionKind::Refresh
        ));
        assert!(enabled(
            &actions,
            crate::ui::work_list_detail::PrActionKind::OpenOnGithub
        ));
        assert!(enabled(
            &actions,
            crate::ui::work_list_detail::PrActionKind::CopyLink
        ));
        assert!(!enabled(
            &actions,
            crate::ui::work_list_detail::PrActionKind::AskQuestion
        ));
        assert!(!enabled(
            &actions,
            crate::ui::work_list_detail::PrActionKind::Explain
        ));
        assert!(!enabled(
            &actions,
            crate::ui::work_list_detail::PrActionKind::CheckOut
        ));
    }

    fn ticket_view_app() -> App {
        let mut app = test_app();
        let ticket = crate::work_index::WorkTicket {
            identifier: "SCA-3165".into(),
            title: Some("image edit reference".into()),
            description: Some("Add the reference.\n- [ ] registry entry".into()),
            state: Some("In Progress".into()),
            assignee: Some("matthias".into()),
            creator: None,
            priority: Some(2),
            cycle: Some("cycle 34".into()),
            group: crate::work_index::TicketGroup::Assigned,
            created_at: None,
            updated_at: None,
            branch: None,
            labels: Vec::new(),
            url: Some("https://linear.app/scalable/issue/SCA-3165".into()),
            parent: None,
            relations: Vec::new(),
        };
        let item = crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
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
            audience: crate::work_index::PrAudience::Unclassified,
            cached_pr_detail: None,
            ticket_ids: vec![ticket.identifier.clone()],
            ticket_title: ticket.title.clone(),
            ticket_state: ticket.state.clone(),
            ticket_details: vec![ticket],
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: Default::default(),
        };
        let mut view = crate::app::state::WorkViewState::new(
            true,
            Some(crate::work_index::Snapshot {
                items: vec![item],
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: std::time::SystemTime::now(),
            }),
        );
        view.projection = crate::app::state::WorkProjection::Tickets;
        app.state.work_view = Some(view);
        app
    }

    fn work_view_collapsed(app: &App, key: &crate::app::state::WorkItemKey) -> Vec<String> {
        let Some(state) = app.state.work_view.as_ref() else {
            return Vec::new();
        };
        let view = state.object_view(key);
        let mut collapsed = view
            .collapsed_sections
            .iter()
            .map(|section| format!("{section:?}"))
            .collect::<Vec<_>>();
        collapsed.sort();
        collapsed
    }

    #[test]
    fn a_closed_board_card_folds_nothing_it_does_not_show() {
        let mut app = ticket_view_app();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 120, 40);
        let key = app
            .visible_ticket_view_keys()
            .first()
            .cloned()
            .expect("a visible ticket");

        app.state.work_view.as_mut().expect("view").ticket_layout =
            crate::app::state::LinearViewLayout::Board;
        app.handle_work_view_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
        assert!(
            work_view_collapsed(&app, &key).is_empty(),
            "the board draws cards, so there is no header on screen to fold"
        );

        // Opening the card puts the detail back on screen and the digit works.
        app.state
            .work_view
            .as_mut()
            .expect("view")
            .board_detail_open = true;
        app.handle_work_view_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
        assert_eq!(work_view_collapsed(&app, &key).len(), 1);
    }

    #[test]
    fn clicking_a_header_in_the_full_work_view_folds_it() {
        let mut app = ticket_view_app();
        let area = ratatui::layout::Rect::new(0, 0, 120, 40);
        app.state.view.terminal_area = area;
        app.state.set_server_mode(Mode::Terminal);
        let key = app
            .visible_ticket_view_keys()
            .first()
            .cloned()
            .expect("a visible ticket");
        let detail = crate::ui::work_view::detail_inner_rect(area);
        let layout = app
            .work_view_detail_layout(&key)
            .expect("a ticket detail layout");
        let (header, section) = layout.sections[1];

        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: detail.x + 2,
            row: detail.y + u16::try_from(header).expect("a header on screen"),
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(
            work_view_collapsed(&app, &key),
            vec![format!("{section:?}")]
        );
    }

    #[test]
    fn work_view_comment_keys_toggle_the_ticket_the_render_shows() {
        let mut app = ticket_view_app();
        let key = app
            .visible_ticket_view_keys()
            .first()
            .cloned()
            .expect("a visible ticket");
        app.state
            .work_item_detail_cache
            .insert(key.clone(), detail_with_comments(&["first", "second"]));
        let expanded = |app: &App| {
            let view = app
                .state
                .work_view
                .as_ref()
                .expect("view")
                .object_view(&key);
            app.state
                .work_item_detail_cache
                .get(&key)
                .map(|detail| {
                    detail
                        .comments
                        .iter()
                        .filter(|comment| {
                            view.comment_is_expanded(crate::ui::work_list_detail::comment_identity(
                                comment,
                            ))
                        })
                        .map(|comment| comment.body.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let press = |app: &mut App, code| {
            app.handle_work_view_key(KeyEvent::new(code, KeyModifiers::empty()))
        };

        press(&mut app, KeyCode::Char('2'));
        assert_eq!(expanded(&app), vec!["second".to_string()]);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(expanded(&app).len(), 2);
        press(&mut app, KeyCode::Char('a'));
        assert!(expanded(&app).is_empty());

        // The board hides the detail until a card is opened, so the digit has
        // nothing to act on there.
        app.state.work_view.as_mut().expect("view").ticket_layout =
            crate::app::state::LinearViewLayout::Board;
        press(&mut app, KeyCode::Char('1'));
        assert!(expanded(&app).is_empty(), "no detail is on screen");
    }

    #[test]
    fn work_view_comment_keys_follow_the_render_when_the_selection_is_filtered_out() {
        let mut app = ticket_view_app();
        let visible = app
            .visible_ticket_view_keys()
            .first()
            .cloned()
            .expect("a visible ticket");
        app.state
            .work_item_detail_cache
            .insert(visible.clone(), detail_with_comments(&["only"]));
        // A refresh can drop the selected item from the filtered list. The
        // render then falls back to the first row, so the keys must too.
        let stale = crate::app::state::WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some("SCA-9999".into()),
        };
        app.state
            .work_item_detail_cache
            .insert(stale.clone(), detail_with_comments(&["gone"]));
        app.state.work_view.as_mut().expect("view").selected = Some(stale.clone());

        app.handle_work_view_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));

        let view = app.state.work_view.as_ref().expect("view");
        assert!(
            view.object_view(&stale).expanded_comments.is_empty(),
            "the filtered-out object is not the one on screen"
        );
        assert_eq!(
            view.object_view(&visible).expanded_comments.len(),
            1,
            "the rendered object is the one that toggled"
        );
    }

    #[test]
    fn ticket_board_keys_navigate_columns_rows_and_open_detail() {
        let mut app = ticket_view_app();
        let second = {
            let view = app.state.work_view.as_ref().expect("ticket view");
            let mut item = view.snapshot.as_ref().expect("snapshot").items[0].clone();
            item.ticket_ids = vec!["SCA-3166".into()];
            item.ticket_title = Some("second ticket".into());
            item.ticket_details[0].identifier = "SCA-3166".into();
            item.ticket_details[0].title = Some("second ticket".into());
            item
        };
        let view = app.state.work_view.as_mut().expect("ticket view");
        view.snapshot.as_mut().expect("snapshot").items.push(second);
        view.ticket_layout = crate::app::state::LinearViewLayout::Board;
        app.state.view.terminal_area = ratatui::layout::Rect::new(26, 2, 53, 7);

        assert!(app.handle_work_view_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty())));
        assert!(app.handle_work_view_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty())));
        assert_eq!(app.state.work_view.as_ref().expect("view").board_column, 2);
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .and_then(|view| view.selected.as_ref())
                .and_then(|key| key.ticket_id.as_deref()),
            Some("SCA-3165")
        );
        app.handle_work_view_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .and_then(|view| view.selected.as_ref())
                .and_then(|key| key.ticket_id.as_deref()),
            Some("SCA-3166")
        );
        assert_eq!(
            app.state.work_view.as_ref().expect("view").board_scroll,
            [0, 0, 1, 0, 0],
            "only the active column scrolls"
        );
        app.handle_work_view_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(
            app.state
                .work_view
                .as_ref()
                .expect("view")
                .board_detail_open
        );
        app.handle_work_view_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(
            !app.state
                .work_view
                .as_ref()
                .expect("view")
                .board_detail_open
        );
        app.handle_work_view_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()));
        assert!(app
            .state
            .work_view
            .as_ref()
            .expect("view")
            .ticket_transition_menu
            .is_some());
    }

    #[test]
    fn ticket_board_mouse_selects_and_double_click_opens_detail() {
        let mut app = ticket_view_app();
        let view = app.state.work_view.as_mut().expect("ticket view");
        view.ticket_layout = crate::app::state::LinearViewLayout::Board;
        view.board_column = 2;
        app.state.view.terminal_area = ratatui::layout::Rect::new(26, 2, 53, 20);
        let layout = crate::ui::work_view::ticket_board_layout(
            &app.state,
            app.state.work_view.as_ref().expect("ticket view"),
            app.state.view.terminal_area,
        );
        let card = layout.cards.first().expect("board card").rect;
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: card.x,
            row: card.y,
            modifiers: KeyModifiers::empty(),
        };
        app.handle_ticket_board_mouse(click);
        assert!(
            !app.state
                .work_view
                .as_ref()
                .expect("view")
                .board_detail_open
        );
        app.handle_ticket_board_mouse(click);
        assert!(
            app.state
                .work_view
                .as_ref()
                .expect("view")
                .board_detail_open
        );
    }

    #[test]
    fn ticket_view_uses_the_configured_default_layout() {
        let mut config = crate::config::Config::default();
        config.linear.default_layout = crate::config::LinearLayoutConfig::Board;
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.toggle_ticket_view();
        assert_eq!(
            app.state.work_view.as_ref().map(|view| view.ticket_layout),
            Some(crate::app::state::LinearViewLayout::Board)
        );
    }

    #[test]
    fn selected_ticket_thread_carries_context_prompt_and_worktree_choice() {
        let mut app = ticket_view_app();
        app.state
            .work_view
            .as_mut()
            .expect("ticket view")
            .ticket_start_menu = Some(crate::app::state::PrCheckoutChoice::NewWorktree);

        app.open_selected_ticket_thread();

        let home = app.state.home.as_ref().expect("thread should open home");
        assert_eq!(home.workspace, crate::app::home::HomeWorkspace::NewWorktree);
        assert_eq!(
            home.ticket
                .as_ref()
                .map(|ticket| ticket.identifier.as_str()),
            Some("SCA-3165")
        );
        assert!(home
            .prompt
            .starts_with("image edit reference\n\nAdd the reference."));
        assert_eq!(
            home.dispatch_plan().expect("dispatch plan").ticket,
            home.ticket
        );
    }

    #[test]
    fn f20_2_full_screen_work_views_never_create_list_surface_tabs() {
        let mut app = test_app();
        app.state.dock_collapsed = true;

        app.toggle_ticket_view();
        assert_eq!(app.state.dock_tab, None);
        assert!(app.state.dock_collapsed);

        app.state.open_dock_surface(crate::app::DockSurface::Files);
        app.toggle_work_view();
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Files));
        assert_eq!(app.state.dock_open_surfaces.len(), 1);
        app.toggle_missive_view();
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Files));
        assert_eq!(app.state.dock_open_surfaces.len(), 1);

        app.toggle_usage_view();
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Files));
        assert_eq!(app.state.dock_open_surfaces.len(), 1);
    }

    #[test]
    fn ticket_transition_is_staged_until_confirmation() {
        let mut app = ticket_view_app();
        app.stage_ticket_transition(crate::app::state::TicketTransitionChoice::Done);
        let pending = app
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.pending_write.as_ref())
            .expect("transition should be staged");
        assert!(matches!(
            pending,
            crate::work_index::WorkItemWrite::TransitionTicket { identifier, state }
                if identifier == "SCA-3165" && state == "Done"
        ));
        app.handle_work_view_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(app
            .state
            .work_view
            .as_ref()
            .is_some_and(|view| view.pending_write.is_none()));
    }

    fn dock_home_test_app(pr_numbers: &[u64]) -> App {
        let mut app = test_app();
        app.state.workspaces = pr_numbers
            .iter()
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        app.state.ensure_test_terminals();
        app.state.active = (!app.state.workspaces.is_empty()).then_some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.dock_home_focused = true;
        for (ws_idx, number) in pr_numbers.iter().enumerate() {
            let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .expect("root terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("valid work context");
        }
        app
    }

    #[tokio::test]
    async fn staging_a_pull_request_action_does_not_run_it() {
        let mut app = dock_home_test_app(&[10, 20]);
        // A program that would fail loudly if it were ever invoked.
        app.work_index_gh_program_override =
            Some(std::path::Path::new("/usr/bin/false").to_path_buf());

        assert!(
            app.handle_dock_home_key(&TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
        );

        let staged = app
            .state
            .dock_pending_write
            .as_ref()
            .expect("merge should be staged");
        assert!(
            matches!(
                staged,
                crate::work_index::WorkItemWrite::MergePullRequest { .. }
            ),
            "{staged:?}"
        );
        assert!(
            staged.describe().contains("squash-merge"),
            "the confirmation must say what it will do: {}",
            staged.describe()
        );
        assert!(
            app.state.dock_write_notice.is_none(),
            "staging must not have run anything"
        );

        // Escape drops it without running.
        assert!(app.handle_dock_home_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.state.dock_pending_write.is_none());
        assert_eq!(app.state.dock_write_notice.as_deref(), Some("cancelled"));
    }

    #[tokio::test]
    async fn a_typed_comment_is_staged_rather_than_posted() {
        let mut app = dock_home_test_app(&[10, 20]);
        app.work_index_gh_program_override =
            Some(std::path::Path::new("/usr/bin/false").to_path_buf());

        assert!(
            app.handle_dock_home_key(&TerminalKey::new(KeyCode::Char('r'), KeyModifiers::empty()))
        );
        for character in "ship it".chars() {
            assert!(app.handle_dock_home_key(&TerminalKey::new(
                KeyCode::Char(character),
                KeyModifiers::empty()
            )));
        }
        assert_eq!(app.state.dock_comment_draft.as_deref(), Some("ship it"));

        assert!(app.handle_dock_home_key(&TerminalKey::new(KeyCode::Enter, KeyModifiers::empty())));
        let staged = app
            .state
            .dock_pending_write
            .as_ref()
            .expect("the comment should be staged, not posted");
        assert!(matches!(
            staged,
            crate::work_index::WorkItemWrite::CommentOnPullRequest { body, .. } if body == "ship it"
        ));
        assert!(app.state.dock_write_notice.is_none());
    }

    #[tokio::test]
    async fn headless_dock_home_key_moves_selection_without_reaching_the_focused_pane() {
        let mut app = dock_home_test_app(&[10, 20]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 2);
        app.state.workspaces[0].insert_test_runtime(pane_id, runtime);

        assert!(app.terminal_input_context().is_some());
        assert_eq!(
            app.state
                .dock_home_selected_row()
                .expect("initial tab")
                .number,
            "10"
        );

        let key = TerminalKey::new(KeyCode::Down, KeyModifiers::empty());
        app.route_client_events(
            vec![
                crate::raw_input::RawInputEvent::Key(key.clone()),
                crate::raw_input::RawInputEvent::Key(
                    key.clone()
                        .with_kind(crossterm::event::KeyEventKind::Repeat),
                ),
                crate::raw_input::RawInputEvent::Key(
                    key.with_kind(crossterm::event::KeyEventKind::Release),
                ),
            ],
            false,
        );

        assert_eq!(
            app.state.dock_home_selected_row().expect("next tab").number,
            "20"
        );
        assert!(
            input_rx.try_recv().is_err(),
            "dock-home presses and repeats must not reach the focused pane"
        );
        assert!(app.input_leases.is_empty());
    }

    #[tokio::test]
    async fn headless_dock_home_key_reaches_pane_outside_the_focused_open_home_tab() {
        for guard in ["collapsed", "other tab", "unfocused"] {
            let mut app = dock_home_test_app(&[10, 20]);
            match guard {
                "collapsed" => app.state.dock_collapsed = true,
                "other tab" => app.state.dock_tab = Some(crate::app::DockSurface::Context),
                "unfocused" => app.state.dock_home_focused = false,
                _ => unreachable!(),
            }
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let (runtime, mut input_rx) =
                crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 2);
            app.state.workspaces[0].insert_test_runtime(pane_id, runtime);

            app.route_client_events(
                vec![crate::raw_input::RawInputEvent::Key(TerminalKey::new(
                    KeyCode::Char('j'),
                    KeyModifiers::empty(),
                ))],
                false,
            );

            assert_eq!(
                app.state
                    .dock_home_selected_row()
                    .expect("unchanged tab")
                    .number,
                "10",
                "guard: {guard}"
            );
            assert_eq!(
                input_rx.try_recv().expect("key forwarded to pane"),
                bytes::Bytes::from_static(b"j"),
                "guard: {guard}"
            );
        }
    }

    #[test]
    fn dock_home_navigate_keys_move_and_enter_jumps_to_the_selected_pane() {
        let mut app = dock_home_test_app(&[10, 20]);

        assert!(app.handle_dock_home_key(&TerminalKey::new(KeyCode::Right, KeyModifiers::empty(),)));
        assert_eq!(
            app.state
                .dock_home_selected_row()
                .expect("second row")
                .number,
            "20"
        );
        assert!(app.handle_dock_home_key(&TerminalKey::new(KeyCode::Enter, KeyModifiers::empty(),)));
        assert_eq!(app.state.active, Some(1));
        assert!(!app.state.dock_home_focused);
    }

    #[test]
    fn dock_home_escape_unfocuses_the_tab_strip() {
        let mut app = dock_home_test_app(&[10]);

        assert!(app.handle_dock_home_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty(),)));
        assert!(!app.state.dock_home_focused);
    }

    #[test]
    fn dock_home_navigate_keys_honor_config_and_do_not_steal_other_focus() {
        let mut app = dock_home_test_app(&[10, 20]);
        let config: crate::config::Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "ctrl+j"
"#,
        )
        .expect("valid config");
        app.state.keybinds = config.keybinds();

        // Bare letters always belong to the pane: the dock is focused over a
        // live shell, so `h`/`j`/`k`/`l` must never be swallowed here.
        assert!(!app
            .handle_dock_home_key(&TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty(),)));
        assert!(!app
            .handle_dock_home_key(&TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty(),)));
        assert!(
            app.handle_dock_home_key(&TerminalKey::new(KeyCode::Char('j'), KeyModifiers::CONTROL,))
        );
        assert_eq!(
            app.state
                .dock_home_selected_row()
                .expect("second row")
                .number,
            "20"
        );

        app.state.dock_tab = Some(crate::app::DockSurface::Editor);
        app.state.dock_editor_focused = true;
        assert!(!app.handle_dock_home_key(&TerminalKey::new(KeyCode::Up, KeyModifiers::empty(),)));
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.dock_home_focused = false;
        assert!(!app.handle_dock_home_key(&TerminalKey::new(KeyCode::Up, KeyModifiers::empty(),)));
    }

    fn app_with_blocked_home_rows(row_count: usize) -> (App, Vec<crate::layout::PaneId>) {
        assert!(row_count > 0);
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("home");
        let mut pane_ids = vec![workspace.tabs[0].root_pane];
        for _ in 1..row_count {
            pane_ids.push(workspace.test_split(Direction::Horizontal));
        }
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        for pane_id in &pane_ids {
            let terminal_id = app.state.workspaces[0]
                .terminal_id(*pane_id)
                .expect("test pane terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("test terminal state")
                .set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);
        }
        app.state.toggle_home();
        (app, pane_ids)
    }

    #[tokio::test]
    async fn clicking_a_home_row_selects_it_and_jumps_to_that_pane() {
        let (mut app, pane_ids) = app_with_blocked_home_rows(3);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));

        let hits = app.state.view.home_row_hit_areas.clone();
        assert_eq!(hits.len(), 3, "every blocked row should be clickable");

        // Click the second row rather than the first, so a jump proves the
        // click chose the row instead of the cursor happening to be there.
        let (index, rect) = hits[1];
        let queue = app.state.home_attention_agents();
        let target = queue[index].pane_id;
        assert_ne!(target, pane_ids[0]);

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: rect.x + 1,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        assert!(app.state.home.is_none(), "a jump leaves home");
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(target));
    }

    #[tokio::test]
    async fn selecting_a_session_in_the_sidebar_leaves_home() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(2);
        app.state
            .workspaces
            .push(crate::workspace::Workspace::test_new("second"));
        app.state.ensure_test_terminals();
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        assert!(app.state.home.is_some(), "home should start open");

        let sidebar = app.state.view.sidebar_rect;
        assert!(sidebar.width > 0, "the sidebar must be visible");
        let target = crate::ui::compute_workspace_card_areas(&app.state, sidebar)
            .into_iter()
            .find(|card| card.ws_idx == 1)
            .expect("a card for the second session");

        // A workspace card commits on release, so the press alone is not a choice.
        for kind in [
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        ] {
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
                crossterm::event::MouseEvent {
                    kind,
                    column: target.rect.x + 2,
                    row: target.rect.y,
                    modifiers: KeyModifiers::NONE,
                },
            ))
            .await;
        }

        assert!(
            app.state.home.is_none(),
            "choosing a session must leave home rather than focus a pane behind it"
        );
    }

    #[tokio::test]
    async fn clicking_off_the_home_rows_neither_jumps_nor_closes_home() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(2);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let before = app.state.workspaces[0].focused_pane_id();

        // The hint row at the bottom of the home frame is not a row.
        let terminal_area = app.state.view.terminal_area;
        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: terminal_area.x + 1,
                row: terminal_area.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        assert!(app.state.home.is_some());
        // Home covers the panes, so the click must not reach the one behind it.
        assert_eq!(app.state.workspaces[0].focused_pane_id(), before);
    }

    /// 1a-4: `Shift+Enter` extends the prompt, plain `Enter` still submits, and
    /// `Esc` closes an open picker before it closes the composer.
    #[tokio::test]
    async fn shift_enter_adds_a_prompt_line_and_escape_closes_the_picker_first() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        {
            let home = app.state.home.as_mut().expect("home");
            home.focus = Some(crate::app::home::HomeFocus::Prompt);
            home.prompt = "first".into();
        }

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::SHIFT))
            .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty()))
            .await;

        assert_eq!(
            app.state.home.as_ref().map(|home| home.prompt.clone()),
            Some("first\nx".to_string()),
            "shift+enter must insert a newline rather than dispatch"
        );

        // Esc unwinds one layer at a time: picker, then composer, then home.
        {
            let home = app.state.home.as_mut().expect("home");
            home.focus = Some(crate::app::home::HomeFocus::Workspace);
            home.picker = Some(crate::app::home::HomePicker::Workspace);
        }
        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;
        let home = app.state.home.as_ref().expect("home stays open");
        assert!(home.picker.is_none(), "esc closes the picker first");
        assert_eq!(home.focus, Some(crate::app::home::HomeFocus::Workspace));

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;
        assert!(app
            .state
            .home
            .as_ref()
            .expect("home stays open")
            .focus
            .is_none());

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;
        assert!(app.state.home.is_none(), "the third esc closes home");
    }

    #[tokio::test]
    async fn clicking_a_home_composer_chip_keeps_home_open_and_opens_its_picker() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(2);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let chip = app
            .state
            .view
            .home_hit_areas
            .iter()
            .find(|hit| hit.target == crate::app::state::HomeHitTarget::Agent)
            .expect("agent chip should be clickable")
            .rect;

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: chip.x,
                row: chip.y,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        let home = app
            .state
            .home
            .as_ref()
            .expect("chip click must not close home");
        assert_eq!(home.focus, Some(crate::app::home::HomeFocus::Agent));
        assert_eq!(home.picker, Some(crate::app::home::HomePicker::Agent));
    }

    #[tokio::test]
    async fn clicking_the_prompt_closes_a_picker_and_the_next_key_edits_the_draft() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        let home = app.state.home.as_mut().expect("home");
        home.prompt = "keep ".into();
        home.focus = Some(crate::app::home::HomeFocus::Agent);
        home.picker = Some(crate::app::home::HomePicker::Agent);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let prompt = app
            .state
            .view
            .home_hit_areas
            .iter()
            .find(|hit| hit.target == crate::app::state::HomeHitTarget::Prompt)
            .expect("prompt should be clickable")
            .rect;

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: prompt.x,
                row: prompt.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('界'), KeyModifiers::empty()))
            .await;

        let home = app.state.home.as_ref().expect("home");
        assert_eq!(home.focus, Some(crate::app::home::HomeFocus::Prompt));
        assert!(home.picker.is_none());
        assert_eq!(home.prompt, "keep 界");
    }

    #[tokio::test]
    async fn clicking_another_home_field_replaces_the_open_picker() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        let home = app.state.home.as_mut().expect("home");
        home.focus = Some(crate::app::home::HomeFocus::Agent);
        home.picker = Some(crate::app::home::HomePicker::Agent);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let model = app
            .state
            .view
            .home_hit_areas
            .iter()
            .find(|hit| hit.target == crate::app::state::HomeHitTarget::Model)
            .expect("model should be clickable")
            .rect;

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: model.x,
                row: model.y,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        let home = app.state.home.as_ref().expect("home");
        assert_eq!(home.focus, Some(crate::app::home::HomeFocus::Model));
        assert_eq!(home.picker, Some(crate::app::home::HomePicker::Model));
    }

    #[tokio::test]
    async fn clicking_blank_home_space_dismisses_the_picker_and_preserves_the_draft() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        let home = app.state.home.as_mut().expect("home");
        home.prompt = "do not lose this".into();
        home.focus = Some(crate::app::home::HomeFocus::Agent);
        home.picker = Some(crate::app::home::HomePicker::Agent);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let area = app.state.view.terminal_area;

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: area.x + 1,
                row: area.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        let home = app.state.home.as_ref().expect("home");
        assert!(home.picker.is_none());
        assert_eq!(home.focus, Some(crate::app::home::HomeFocus::Agent));
        assert_eq!(home.prompt, "do not lose this");
    }

    #[tokio::test]
    async fn clicking_new_task_in_the_lens_focuses_the_preserved_prompt() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        let home = app.state.home.as_mut().expect("home");
        home.prompt = "continue here".into();
        home.focus = None;
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let new_task = app
            .state
            .view
            .home_hit_areas
            .iter()
            .find(|hit| hit.target == crate::app::state::HomeHitTarget::NewTask)
            .expect("new task should be clickable")
            .rect;

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: new_task.x,
                row: new_task.y,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        let home = app.state.home.as_ref().expect("home");
        assert_eq!(home.focus, Some(crate::app::home::HomeFocus::Prompt));
        assert_eq!(home.prompt, "continue here");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn n_opens_the_composer_from_the_home_lens() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        app.state.home.as_mut().expect("home").focus = None;

        app.handle_key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::empty()))
            .await;

        assert_eq!(
            app.state.home.as_ref().and_then(|home| home.focus),
            Some(crate::app::home::HomeFocus::Prompt)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tab_closes_a_picker_and_moves_focus_without_opening_the_next_picker() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        let home = app.state.home.as_mut().expect("home");
        home.focus = Some(crate::app::home::HomeFocus::Agent);
        home.picker = Some(crate::app::home::HomePicker::Agent);

        app.handle_key(TerminalKey::new(KeyCode::Tab, KeyModifiers::empty()))
            .await;

        let home = app.state.home.as_ref().expect("home");
        assert_eq!(home.focus, Some(crate::app::home::HomeFocus::Model));
        assert!(home.picker.is_none());
    }

    #[test]
    fn home_row_hit_areas_are_dropped_as_soon_as_home_closes() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(2);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        assert!(!app.state.view.home_row_hit_areas.is_empty());

        app.state.clear_home();
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));

        assert!(app.state.view.home_row_hit_areas.is_empty());
        assert_eq!(app.state.home_row_at(30, 3), None);
    }

    #[test]
    fn opening_home_and_inbox_closes_the_other_overlay() {
        let mut app = test_app();

        app.state.toggle_inbox();
        app.state.toggle_home();
        assert!(app.state.home.is_some());
        assert!(app.state.inbox.is_none());

        app.state.toggle_inbox();
        assert!(app.state.home.is_none());
        assert!(app.state.inbox.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pressing_enter_after_the_selected_home_pane_closes_leaves_home_open() {
        let (mut app, pane_ids) = app_with_blocked_home_rows(1);
        app.state.workspaces[0].tabs[0].panes.remove(&pane_ids[0]);
        app.state.home.as_mut().expect("home overlay").focus = None;

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert!(app.state.home.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pressing_enter_on_an_existing_home_pane_closes_home() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);
        app.state.home.as_mut().expect("home overlay").focus = None;

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert!(app.state.home.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pressing_enter_in_the_home_reply_sends_and_returns_focus_to_the_queue() {
        let (mut app, _terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.state.toggle_home();
        app.state.home.as_mut().expect("home overlay").focus = None;
        app.handle_key(TerminalKey::new(KeyCode::Tab, KeyModifiers::empty()))
            .await;
        assert_eq!(
            app.state.home.as_ref().and_then(|home| home.focus),
            Some(crate::app::home::HomeFocus::Reply)
        );

        for character in "answer".chars() {
            app.handle_key(TerminalKey::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            ))
            .await;
        }
        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert_eq!(rx.recv().await.expect("reply bytes").as_ref(), b"answer\r");
        assert_eq!(
            app.state.home.as_ref().and_then(|home| home.focus),
            None,
            "a sent reply returns to queue focus"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_pending_human_draft_rejects_a_home_reply_without_changing_the_draft() {
        let (mut app, _terminal_id, mut rx) = terminal_app_with_blocked_hook();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state
            .pending_human_drafts
            .insert(pane_id, "human draft byte exact".into());
        app.state.toggle_home();
        let home = app.state.home.as_mut().expect("home overlay");
        home.focus = Some(crate::app::home::HomeFocus::Reply);
        home.reply = "answer anyway".into();

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert_eq!(
            app.state
                .pending_human_drafts
                .get(&pane_id)
                .map(String::as_bytes),
            Some(b"human draft byte exact".as_slice())
        );
        assert_eq!(
            app.state
                .home
                .as_ref()
                .and_then(|home| home.reply_error.as_deref()),
            Some("human draft pending · clear it in the pane")
        );
        assert!(
            rx.try_recv().is_err(),
            "the protected pane must not receive bytes"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pressing_escape_leaves_the_composer_before_closing_home() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert!(app.state.home.is_some());
        assert_eq!(app.state.home.as_ref().and_then(|home| home.focus), None);

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;
        assert!(app.state.home.is_none());
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn clicking_detach_jumps_to_the_selected_pane_and_closes_home() {
        let (mut app, pane_ids) = app_with_blocked_home_rows(2);
        app.state.home.as_mut().expect("home overlay").focus = None;
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let detach = app
            .state
            .view
            .home_hit_areas
            .iter()
            .find(|hit| hit.target == crate::app::state::HomeHitTarget::Detach)
            .expect("detach should be clickable")
            .rect;
        let target = pane_ids[0];

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: detach.x,
                row: detach.y,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        assert!(app.state.home.is_none());
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(target));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn home_cursor_moves_with_vi_and_arrow_keys_without_wrapping() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(3);
        app.state.home.as_mut().expect("home overlay").focus = None;
        let selected = |app: &App| {
            app.state
                .home
                .as_ref()
                .expect("home overlay")
                .selected(&app.state.home_attention_agents())
        };

        assert_eq!(selected(&app), 0);
        app.handle_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 1);
        app.handle_key(TerminalKey::new(KeyCode::Down, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 2);
        app.handle_key(TerminalKey::new(KeyCode::Down, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 2);
        app.handle_key(TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 1);
        app.handle_key(TerminalKey::new(KeyCode::Up, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 0);
        app.handle_key(TerminalKey::new(KeyCode::Up, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn home_prompt_typing_is_consumed_by_the_composer() {
        let (mut app, _terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.state.toggle_home();

        let target = app
            .handle_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()))
            .await;

        assert!(app.state.home.is_some());
        assert_eq!(
            app.state.home.as_ref().map(|home| home.prompt.as_str()),
            Some("j")
        );
        assert!(target.is_none(), "the prompt key stays inside home");
        let _ = &mut rx;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn home_keeps_queue_navigation_keys_inside_the_overlay() {
        let (mut app, _terminal_id, _rx) = terminal_app_with_blocked_hook();
        app.state.toggle_home();
        app.state.home.as_mut().expect("home overlay").focus = None;

        for code in [
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::Up,
            KeyCode::Char('k'),
        ] {
            assert!(
                app.handle_home_key_event(KeyEvent::new(code, KeyModifiers::empty())),
                "{code:?} browses rather than reaching the pane"
            );
            assert!(app.state.home.is_some());
        }

        // Esc spends itself closing home rather than travelling on.
        assert!(app.handle_home_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())));
        assert!(app.state.home.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn headless_home_keys_follow_the_composer_input_rule() {
        let (mut app, _terminal_id, _rx) = terminal_app_with_blocked_hook();
        app.state.toggle_home();

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(TerminalKey::new(
                KeyCode::Char('x'),
                KeyModifiers::empty(),
            ))],
            false,
        );

        assert_eq!(
            app.state.home.as_ref().map(|home| home.prompt.as_str()),
            Some("x")
        );
    }

    #[tokio::test]
    async fn paste_routes_to_rename_modal_input() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        app.state.name_input = "2".into();
        app.state.name_input_replace_on_type = true;

        app.handle_paste("feature/logs".into()).await;

        assert_eq!(app.state.name_input, "feature/logs");
        assert!(!app.state.name_input_replace_on_type);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn paste_retires_blocked_hook_authority_after_forwarding() {
        let (mut app, terminal_id, mut rx) = terminal_app_with_blocked_hook();

        app.handle_paste("continue".into()).await;

        assert!(rx.try_recv().is_ok());
        assert_blocked_hook_retired(&app, &terminal_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_commit_paths_retire_blocked_hook_authority_after_forwarding() {
        let (mut app, terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.handle_text_commit("continue".into()).await;
        assert!(rx.try_recv().is_ok());
        assert_blocked_hook_retired(&app, &terminal_id);

        let (mut app, terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.handle_text_commit_headless("continue");
        assert!(rx.try_recv().is_ok());
        assert_blocked_hook_retired(&app, &terminal_id);
    }

    #[tokio::test]
    async fn local_forwarded_mouse_wheel_keeps_full_lifecycle_blocked_hold() {
        let (mut app, terminal_id, mut rx) =
            terminal_app_with_mouse_reporting_blocked_hook(b"\x1b[?1000h\x1b[?1006h");
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 26));
        let info = app.state.view.pane_infos[0].clone();

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            info.inner_rect.x + 2,
            info.inner_rect.y + 2,
        ));

        assert!(
            rx.try_recv().is_ok(),
            "mouse-reporting wheel reaches the pane"
        );
        assert_blocked_hook_held(&app, &terminal_id);
    }

    #[tokio::test]
    async fn local_forwarded_mouse_motion_and_button_press_keep_hold_until_key_input() {
        let (mut app, terminal_id, mut rx) =
            terminal_app_with_mouse_reporting_blocked_hook(b"\x1b[?1003h\x1b[?1006h");
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 26));
        let info = app.state.view.pane_infos[0].clone();
        let column = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 2;

        app.handle_mouse(mouse(MouseEventKind::Moved, column, row));

        assert!(rx.try_recv().is_ok(), "mouse motion reaches the pane");
        assert_blocked_hook_held(&app, &terminal_id);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row));

        assert!(rx.try_recv().is_ok(), "button press reaches the pane");
        assert_blocked_hook_held(&app, &terminal_id);

        app.handle_key(TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty()))
            .await;

        assert!(rx.try_recv().is_ok(), "key press reaches the pane");
        assert_blocked_hook_retired(&app, &terminal_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_text_and_paste_are_consumed_by_subgroup_picker_before_home() {
        let (mut app, _terminal_id, mut pane_input) = terminal_app_with_blocked_hook();
        app.state.toggle_home();
        let home_prompt_before = app
            .state
            .home
            .as_ref()
            .map(|home| home.prompt.clone())
            .expect("home");
        app.state.sidebar_subgroup_picker = Some(crate::app::state::SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 0,
            anchor: (7, 4),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });

        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Text(
                crate::input::TextCommit::new("ap"),
            ))
            .await
        );
        assert!(
            app.handle_raw_input_event(crate::raw_input::RawInputEvent::Paste("i".into()))
                .await
        );

        assert_eq!(
            app.state
                .sidebar_subgroup_picker
                .as_ref()
                .map(|picker| picker.filter.query.as_str()),
            Some("api")
        );
        assert_eq!(
            app.state.home.as_ref().map(|home| home.prompt.as_str()),
            Some(home_prompt_before.as_str())
        );
        assert!(
            pane_input.try_recv().is_err(),
            "picker-owned text must not reach the pane"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_subgroup_picker_takes_keys_before_focused_notepad() {
        let (mut app, _terminal_id, mut pane_input) = terminal_app_with_blocked_hook();
        app.state.notepad.enabled = true;
        app.state.notepad.focused = true;
        let note_before = app.state.notepad.body().to_string();
        app.state.sidebar_subgroup_picker = Some(crate::app::state::SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 0,
            anchor: (7, 4),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });

        let target = app
            .handle_key(TerminalKey::new(KeyCode::Char('a'), KeyModifiers::empty()))
            .await;

        assert!(target.is_none());
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

    #[tokio::test(flavor = "current_thread")]
    async fn symphony_overlay_blocks_text_paste_and_headless_keys() {
        let (mut app, _terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.state.toggle_symphony();

        app.handle_text_commit("hidden command\n".into()).await;
        app.handle_text_commit_headless("hidden command\n");
        app.handle_paste("hidden command\n".into()).await;
        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste(
                "hidden command\n".into(),
            )],
            false,
        );
        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(TerminalKey::new(
                KeyCode::Char('x'),
                KeyModifiers::empty(),
            ))],
            false,
        );

        assert!(rx.try_recv().is_err());
        assert!(app.state.symphony_detail.is_some());
    }

    #[tokio::test]
    async fn paste_routes_to_keybind_help_query_only_when_searching() {
        let mut app = test_app();
        app.state.set_server_mode(Mode::KeybindHelp);
        app.handle_paste("ignored".into()).await;
        assert!(app.state.keybind_help.query.is_empty());

        app.state.keybind_help.search_focused = true;
        app.state.keybind_help.scroll = 3;
        app.handle_paste("work\nspace".into()).await;

        assert_eq!(app.state.keybind_help.query, "workspace");
        assert_eq!(app.state.keybind_help.scroll, 0);
    }

    #[tokio::test]
    async fn paste_routes_to_new_linked_worktree_input() {
        let mut app = test_app();
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::NewLinkedWorktree);
        app.state.name_input = "generated-branch".into();
        app.state.name_input_replace_on_type = true;
        app.state.worktree_create = Some(crate::app::state::WorktreeCreateState {
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

        app.handle_paste("feature/linear-302".into()).await;

        assert_eq!(app.state.name_input, "feature/linear-302");
        assert_eq!(
            app.state
                .worktree_create
                .as_ref()
                .map(|create| create.branch.as_str()),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn modal_paste_shortcut_matches_platform_primary_v() {
        #[cfg(target_os = "macos")]
        let modifiers = KeyModifiers::SUPER;
        #[cfg(not(target_os = "macos"))]
        let modifiers = KeyModifiers::CONTROL;

        assert!(is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('v'),
            modifiers
        )));
        assert!(is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('V'),
            modifiers | KeyModifiers::SHIFT
        )));
        assert!(!is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::ALT
        )));
    }

    #[test]
    fn ctrl_v_pastes_into_rename_and_worktree_dialogs_for_local_and_attached_routes() {
        #[cfg(target_os = "macos")]
        let modifiers = KeyModifiers::SUPER;
        #[cfg(not(target_os = "macos"))]
        let modifiers = KeyModifiers::CONTROL;
        let key = KeyEvent::new(KeyCode::Char('v'), modifiers);

        for route in ["local", "attached"] {
            let mut rename = test_app();
            rename
                .state
                .open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
            assert!(rename.paste_clipboard_shortcut_for_input_owner(
                rename.state.input_owner(),
                &key,
                || Some("renamed".into()),
            ));
            assert_eq!(rename.state.name_input, "renamed", "{route} rename");

            let mut create = test_app();
            create
                .state
                .open_client_overlay(crate::app::state::ClientOverlay::NewLinkedWorktree);
            assert!(create.paste_clipboard_shortcut_for_input_owner(
                create.state.input_owner(),
                &key,
                || Some("feature/client-owner".into()),
            ));
            assert_eq!(
                create.state.name_input, "feature/client-owner",
                "{route} create"
            );

            let mut open = test_app();
            open.state.worktree_open = Some(crate::app::state::WorktreeOpenState {
                source_workspace_id: "source".into(),
                source_existing_membership: None,
                source_checkout_path: "/repo/herdr".into(),
                source_repo_root: "/repo/herdr".into(),
                repo_key: "repo-key".into(),
                repo_name: "herdr".into(),
                entries: Vec::new(),
                selected: 0,
                query: String::new(),
                search_focused: true,
                error: None,
            });
            open.state
                .open_client_overlay(crate::app::state::ClientOverlay::OpenExistingWorktree);
            assert!(open.paste_clipboard_shortcut_for_input_owner(
                open.state.input_owner(),
                &key,
                || Some("existing".into()),
            ));
            assert_eq!(
                open.state
                    .worktree_open
                    .as_ref()
                    .map(|state| state.query.as_str()),
                Some("existing"),
                "{route} open"
            );
        }
    }

    #[test]
    fn modal_paste_target_is_active_only_for_text_inputs() {
        let mut state = AppState::test_new();

        state.open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        assert!(modal_paste_target_active(&state));
        state.close_client_overlay();

        state.set_server_mode(Mode::Navigator);
        state.navigator.search_focused = false;
        assert!(!modal_paste_target_active(&state));
        state.navigator.search_focused = true;
        assert!(modal_paste_target_active(&state));

        state.set_server_mode(Mode::KeybindHelp);
        state.keybind_help.search_focused = false;
        assert!(!modal_paste_target_active(&state));
        state.keybind_help.search_focused = true;
        assert!(modal_paste_target_active(&state));

        state.open_client_overlay(crate::app::state::ClientOverlay::ConfirmClose);
        assert!(!modal_paste_target_active(&state));
    }

    fn app_with_missive_view() -> App {
        let mut app = test_app();
        let conversation = crate::work_index::MissiveConversation {
            id: "sample".into(),
            subject: "Billing question".into(),
            app_url: "missive://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            team: None,
            assignees: Vec::new(),
            last_activity_at: Some(std::time::SystemTime::UNIX_EPOCH),
            closed: false,
            labels: Vec::new(),
            pane_bound: false,
            messages: Vec::new(),
            notes: Vec::new(),
            drafts: Vec::new(),
            posts: Vec::new(),
        };
        let mut view = crate::app::state::WorkViewState::new(
            true,
            Some(crate::work_index::Snapshot {
                items: Vec::new(),
                conversations: vec![conversation],
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: std::time::SystemTime::UNIX_EPOCH,
            }),
        );
        view.projection = crate::app::state::WorkProjection::Missive;
        app.state.work_view = Some(view);
        app
    }

    #[test]
    fn explicit_work_view_refresh_bypasses_only_the_visible_provider_cache() {
        let mut app = app_with_missive_view();
        let later = std::time::Instant::now() + std::time::Duration::from_secs(60);
        app.next_work_index_refresh = later;

        app.handle_work_view_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty()));

        assert!(app.next_work_index_refresh < later);
        assert_eq!(
            app.work_index_cache_bypass,
            crate::work_index::WorkIndexCacheBypass {
                missive: true,
                ..Default::default()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn successful_reviewer_assignment_refreshes_pr_detail_with_cache_bypass() {
        use std::os::unix::fs::PermissionsExt;

        let (mut app, key) = pr_action_test_app();
        let fixture_dir = std::env::temp_dir().join(format!(
            "herdr-reviewer-refresh-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&fixture_dir).expect("create fixture directory");
        let log = fixture_dir.join("detail.log");
        let argv_log = fixture_dir.join("argv.log");
        let gh = fixture_dir.join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "pr edit 7 --add-reviewer grace -R owner/repo") exit 0 ;;
  "pr view 7 --repo owner/repo --json "*)
    printf '%s\n' detail >> '{}'
    printf '%s' '{{"number":7,"title":"Detail","url":"https://github.com/owner/repo/pull/7","reviews":[{{"author":{{"login":"grace"}}}}]}}'
    ;;
  "api repos/owner/repo/issues/7/timeline"*) printf '%s' '[]' ;;
  "api graphql"*) printf '%s' '{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[]}}}}}}}}}}' ;;
  *) exit 42 ;;
esac
"#,
                argv_log.display(),
                log.display()
            ),
        )
        .expect("write fake gh");
        let mut permissions = std::fs::metadata(&gh)
            .expect("fake gh metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).expect("make fake gh executable");
        app.work_index_gh_program_override = Some(gh);
        app.work_index_provider_cache_root_override = Some(fixture_dir.clone());

        app.add_selected_pr_reviewer("grace".into());

        assert!(app.work_index_cache_bypass.github);
        assert!(!app.work_index_cache_bypass.linear);
        // The scheduled index refresh invalidates detail entries before the
        // focused surface hydrates the selected PR again.
        app.state.work_item_detail_cache.clear();
        app.start_work_item_detail_refresh_if_due(
            std::time::Instant::now(),
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            true,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let event = loop {
            match app.event_rx.try_recv() {
                Ok(event) => break event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!(
                    "detail refresh event missing: {error}; argv: {}",
                    std::fs::read_to_string(&argv_log).unwrap_or_default()
                ),
            }
        };
        let crate::events::AppEvent::WorkItemDetailRefreshed {
            generation,
            details,
        } = event
        else {
            panic!("expected detail refresh event");
        };
        assert!(app.handle_work_item_detail_refreshed(generation, details));
        assert_eq!(
            std::fs::read_to_string(log).expect("read detail counter"),
            "detail\n"
        );
        assert!(app
            .state
            .work_item_detail_cache
            .get(&key)
            .is_some_and(|detail| detail.reviewers == ["grace"]));
        let _ = std::fs::remove_dir_all(fixture_dir);
    }

    #[test]
    fn missive_view_copies_app_url_without_opening_a_browser() {
        let mut app = app_with_missive_view();
        assert!(app.handle_work_view_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::empty(),)));
        assert_eq!(
            app.state.request_clipboard_write,
            Some(b"missive://mail.missiveapp.com/#inbox/conversations/sample".to_vec())
        );
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .and_then(|view| view.hint.as_deref()),
            Some("Missive link copied")
        );

        app.state.request_clipboard_write = None;
        let view = app.state.work_view.as_mut().expect("work view");
        view.projection = crate::app::state::WorkProjection::PullRequests;
        view.hint = None;
        app.handle_work_view_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::empty()));
        assert!(app.state.request_clipboard_write.is_none());
        assert!(app
            .state
            .work_view
            .as_ref()
            .is_some_and(|view| view.hint.is_none()));
    }

    #[test]
    fn missive_selection_schedules_selected_detail_hydration() {
        let mut app = app_with_missive_view();
        let mut second = app
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.snapshot.as_ref())
            .and_then(|snapshot| snapshot.conversations.first())
            .cloned()
            .expect("first conversation");
        second.id = "second".into();
        second.subject = "Second conversation".into();
        app.state
            .work_view
            .as_mut()
            .and_then(|view| view.snapshot.as_mut())
            .expect("work snapshot")
            .conversations
            .push(second);
        app.next_work_index_refresh =
            std::time::Instant::now() + std::time::Duration::from_secs(60);
        app.state
            .work_view
            .as_mut()
            .expect("Missive view")
            .missive_detail_scroll = 9;

        app.move_missive_view_selection(1);

        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .and_then(|view| view.selected_missive.as_deref()),
            Some("second")
        );
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .map(|view| view.missive_detail_scroll),
            Some(0)
        );
        assert!(app.next_work_index_refresh <= std::time::Instant::now());
    }

    #[test]
    fn missive_page_keys_scroll_detail_without_moving_the_conversation() {
        let mut app = app_with_missive_view();
        app.handle_work_view_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::empty()));
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .map(|view| (view.selected_missive.as_deref(), view.missive_detail_scroll)),
            Some((None, 5))
        );
        app.handle_work_view_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty()));
        assert_eq!(
            app.state
                .work_view
                .as_ref()
                .map(|view| view.missive_detail_scroll),
            Some(0)
        );
    }

    #[test]
    fn missive_start_thread_opens_home_with_conversation_context() {
        let mut app = app_with_missive_view();
        app.handle_work_view_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()));
        assert!(app.state.work_view.as_ref().is_some_and(|view| {
            view.missive_start_menu == Some(crate::app::state::PrCheckoutChoice::CurrentCheckout)
        }));
        app.handle_work_view_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        let home = app.state.home.as_ref().expect("home composer");
        assert_eq!(
            home.missive
                .as_ref()
                .map(|context| context.app_url.as_str()),
            Some("missive://mail.missiveapp.com/#inbox/conversations/sample")
        );
        assert_eq!(
            home.missive
                .as_ref()
                .map(|context| context.web_url.as_str()),
            Some("https://mail.missiveapp.com/#inbox/conversations/sample")
        );
        assert!(home.prompt.contains("Billing question"));
        assert!(home.prompt.contains("https://mail.missiveapp.com"));
    }
}
