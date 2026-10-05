use std::{
    fs, io,
    io::Write,
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Direction;

use crate::{
    app::{
        state::{AppState, Mode},
        App,
    },
    input::TerminalKey,
    layout::NavDirection,
    terminal::TerminalRuntimeRegistry,
};

#[cfg(test)]
pub(crate) fn terminal_direct_navigation_action(
    state: &AppState,
    key: TerminalKey,
) -> Option<NavigateAction> {
    action_for_key(state, key, BindingDispatch::Direct)
}

pub(crate) fn terminal_direct_non_indexed_navigation_action(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    non_indexed_action_for_key(state, key, BindingDispatch::Direct)
}

pub(crate) fn terminal_direct_indexed_navigation_action(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    indexed_navigation_action(state, key, BindingDispatch::Direct)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionContext {
    Direct,
    Prefix,
    Navigate,
}

impl App {
    fn cancel_copy_mode_if_active(&mut self) {
        if self.state.copy_mode.is_some() {
            self.state.cancel_copy_mode(&self.terminal_runtimes);
        }
    }

    pub(crate) fn handle_prefix_key(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        self.state.update_dismissed = true;

        if matches!(key.code, KeyCode::Modifier(_)) {
            return;
        }

        if self.state.is_prefix_key(&raw_key) {
            if self.state.copy_mode_pane_is_focused() {
                self.state.cancel_copy_mode(&self.terminal_runtimes);
            }
            if !self.pass_through_key_to_focused_pane(raw_key) {
                leave_command_mode(&mut self.state);
            }
            return;
        }

        if key.code == KeyCode::Esc {
            leave_command_mode(&mut self.state);
            return;
        }

        if key.code == KeyCode::Char('h') && key.modifiers == KeyModifiers::CONTROL {
            self.state.toggle_loop_run_history();
            leave_command_mode(&mut self.state);
            return;
        }

        match prefix_binding_for_key(&self.state, &raw_key) {
            Some(PrefixBindingMatch::Action(action)) => self.execute_prefix_key_action(action),
            Some(PrefixBindingMatch::Command(binding)) => {
                self.cancel_copy_mode_if_active();
                self.launch_custom_command(binding, ActionContext::Prefix);
            }
            Some(PrefixBindingMatch::UserAction(index)) => {
                self.state.request_user_action = Some(index);
                leave_command_mode(&mut self.state);
            }
            None => leave_command_mode(&mut self.state),
        }
    }

    fn execute_prefix_key_action(&mut self, action: NavigateAction) {
        if action == NavigateAction::EditScrollback {
            let previous_mode = self.state.server_mode();
            self.cancel_copy_mode_if_active();
            self.launch_focused_scrollback_editor();
            finish_action_context(&mut self.state, ActionContext::Prefix, previous_mode);
        } else if action == NavigateAction::CopyMode {
            self.cancel_copy_mode_if_active();
            self.execute_tui_navigate_action(action, ActionContext::Prefix);
        } else if copy_mode_survives_prefix_action(action) {
            self.execute_tui_navigate_action(action, ActionContext::Prefix);
            if self.state.copy_mode.is_some() {
                self.state.sync_copy_mode_with_focus();
            }
        } else {
            self.cancel_copy_mode_if_active();
            self.execute_tui_navigate_action(action, ActionContext::Prefix);
        }
        self.selection_autoscroll_deadline = None;
    }

    pub(crate) fn handle_navigate_key(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        self.state.update_dismissed = true;

        if key.code == KeyCode::Esc || self.state.is_prefix_key(&raw_key) {
            leave_navigate_mode(&mut self.state);
            return;
        }

        if self
            .state
            .keybinds
            .navigate
            .workspace_up
            .matches_direct_key(&raw_key)
        {
            self.state.move_selected_workspace_by_visible_delta(-1);
            return;
        }
        if self
            .state
            .keybinds
            .navigate
            .workspace_down
            .matches_direct_key(&raw_key)
        {
            self.state.move_selected_workspace_by_visible_delta(1);
            return;
        }

        if let Some(action) = navigate_reserved_action_for_key(&self.state, &raw_key) {
            self.execute_tui_navigate_action(action, ActionContext::Navigate);
            return;
        }

        if let Some(action) = navigate_mode_non_indexed_action_for_key(&self.state, &raw_key) {
            if action == NavigateAction::EditScrollback {
                self.launch_focused_scrollback_editor();
            } else {
                self.execute_tui_navigate_action(action, ActionContext::Navigate);
            }
            self.selection_autoscroll_deadline = None;
            return;
        }

        if let Some(binding) = command_for_key(&self.state, &raw_key, BindingDispatch::Prefix) {
            self.launch_custom_command(binding, ActionContext::Navigate);
            return;
        }

        if let Some(index) = user_action_for_key(&self.state, &raw_key, BindingDispatch::Prefix) {
            self.state.request_user_action = Some(index);
            leave_navigate_mode(&mut self.state);
            return;
        }

        if let Some(action) = navigate_mode_indexed_action_for_key(&self.state, &raw_key) {
            self.execute_tui_navigate_action(action, ActionContext::Navigate);
            self.selection_autoscroll_deadline = None;
        }
    }

    pub(crate) fn execute_tui_navigate_action(
        &mut self,
        action: NavigateAction,
        context: ActionContext,
    ) {
        if !matches!(
            action,
            NavigateAction::PreviousWindow | NavigateAction::NextWindow
        ) {
            self.invalidate_window_cycle_snapshot();
        }
        let previous_mode = self.state.server_mode();
        match action {
            NavigateAction::NewWorkspace => {
                self.begin_tui_workspace_create("tui.key.workspace.create");
            }
            NavigateAction::NewThread => {
                // The picker takes keys through the sidebar's input path, so
                // opening it without giving the sidebar the keyboard would draw
                // a filter box that nothing can type into.
                self.state.focus_client_on_sidebar();
                self.state.open_sidebar_new_thread();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NewAgentDock => {
                leave_navigate_mode(&mut self.state);
                self.state.open_spawn_dock();
                if let Some(dock) = self.state.spawn_dock.as_ref() {
                    self.state
                        .sidebar_presentation
                        .save_spawn_dock_draft(Some(dock.draft()));
                }
            }
            NavigateAction::OpenAgentFinder => {
                leave_navigate_mode(&mut self.state);
                self.state.open_agent_finder();
            }
            NavigateAction::NewWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context).filter(|idx| {
                    workspace_can_start_worktree_action(&self.state, &self.terminal_runtimes, *idx)
                }) {
                    self.state.request_new_linked_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context).filter(|idx| {
                    workspace_can_start_worktree_action(&self.state, &self.terminal_runtimes, *idx)
                }) {
                    self.state.request_open_existing_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::RemoveWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    self.state.request_remove_linked_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::RenameWorkspace => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    super::modal::open_rename_workspace(
                        &mut self.state,
                        &self.terminal_runtimes,
                        ws_idx,
                    );
                }
            }
            NavigateAction::CloseWorkspace => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    self.state.selected = ws_idx;
                    if self.state.confirm_close {
                        super::modal::open_confirm_close(&mut self.state);
                    } else {
                        self.close_workspace_idx_with_group_via_api(ws_idx);
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::SwitchWorkspace(idx) => {
                if let Some(ws_idx) = self.state.workspace_at_visible_position(idx) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::SwitchTab(idx) => {
                if self
                    .state
                    .active
                    .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| idx < ws.tabs.len())
                {
                    self.focus_tab_idx_via_api(idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::FocusAgent(idx) => {
                if let Some((ws_idx, pane_id)) = self.agent_entry_target(idx) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    leave_navigate_mode(&mut self.state);
                    self.state.ensure_agent_row_visible(ws_idx, pane_id);
                }
            }
            NavigateAction::WorkspacePicker => {
                self.state.begin_workspace_picker_presentation();
                self.state.set_server_mode(Mode::Navigate);
            }
            NavigateAction::PreviousWorkspace => {
                if let Some(ws_idx) = self.relative_visible_workspace(-1) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextWorkspace => {
                if let Some(ws_idx) = self.relative_visible_workspace(1) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::PreviousAgent => {
                if let Some((_idx, entry)) = self.relative_agent_entry(false) {
                    self.focus_agent_panel_entry(entry);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextAgent => {
                if let Some((_idx, entry)) = self.relative_agent_entry(true) {
                    self.focus_agent_panel_entry(entry);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextReviewAgent => {
                if let Some((ws_idx, pane_id)) = next_review_agent_target(&self.state) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    self.state.dock_home_focused = false;
                    self.state.dock_diff_focused = false;
                    self.state.dock_files_focused = false;
                    self.state.dock_agents_focused = false;
                    self.state.dock_hosts_focused = false;
                }
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NewTab => {
                if self.state.active.is_some() {
                    if self.state.prompt_new_tab_name {
                        super::modal::open_new_tab_dialog(&mut self.state);
                    } else {
                        self.runtime_tab_create(
                            "tui.key.tab.create",
                            crate::api::schema::TabCreateParams {
                                workspace_id: None,
                                cwd: None,
                                focus: true,
                                label: None,
                                env: Default::default(),
                                work_context: None,
                            },
                        );
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::RenameTab => {
                super::modal::open_rename_active_tab(&mut self.state, false)
            }
            NavigateAction::ToggleTabPrio => {
                if toggle_tab_prio(&mut self.state, context) {
                    self.schedule_session_save();
                    if context == ActionContext::Navigate {
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::TogglePrioPanel => {
                self.state.toggle_prio_panel();
                self.schedule_session_save();
                if context == ActionContext::Navigate {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::ToggleBlockedFilter => {
                self.state.blocked_filter = !self.state.blocked_filter;
                self.state.workspace_scroll = crate::ui::normalized_workspace_scroll(
                    &self.state,
                    self.state.view.sidebar_rect,
                    self.state.workspace_scroll,
                );
                if context == ActionContext::Navigate {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::PreviousTab => {
                if let Some(tab_idx) = self.relative_tab(-1) {
                    self.focus_tab_idx_via_api(tab_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextTab => {
                if let Some(tab_idx) = self.relative_tab(1) {
                    self.focus_tab_idx_via_api(tab_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::MoveTabPrevious => {
                if let Some((ws_idx, source, insert)) = self.active_tab_move(-1) {
                    self.move_tab_via_api(ws_idx, source, insert);
                }
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::MoveTabNext => {
                if let Some((ws_idx, source, insert)) = self.active_tab_move(1) {
                    self.move_tab_via_api(ws_idx, source, insert);
                }
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::PreviousWindow => {
                self.focus_relative_window(false);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NextWindow => {
                self.focus_relative_window(true);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NextBlockedWindow => {
                self.focus_next_blocked_window();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CloseTab => {
                self.close_active_tab_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::RenamePane => {
                if let Some(pane_id) = self
                    .state
                    .active
                    .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                    .and_then(|ws| ws.focused_pane_id())
                {
                    super::modal::open_rename_pane(&mut self.state, pane_id);
                }
            }
            NavigateAction::FocusPaneLeft => {
                self.focus_pane_direction_in_context(NavDirection::Left, context)
            }
            NavigateAction::FocusPaneDown => {
                self.focus_pane_direction_in_context(NavDirection::Down, context)
            }
            NavigateAction::FocusPaneUp => {
                self.focus_pane_direction_in_context(NavDirection::Up, context)
            }
            NavigateAction::FocusPaneRight => {
                self.focus_pane_direction_in_context(NavDirection::Right, context)
            }
            NavigateAction::SwapPaneLeft => {
                self.swap_pane_direction_via_api(NavDirection::Left);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneDown => {
                self.swap_pane_direction_via_api(NavDirection::Down);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneUp => {
                self.swap_pane_direction_via_api(NavDirection::Up);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneRight => {
                self.swap_pane_direction_via_api(NavDirection::Right);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitVertical => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Right);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitHorizontal => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Down);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitLeft => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Left);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitUp => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Up);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ClosePane => {
                if !self.close_focused_pane_via_api_requires_confirmation() {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::EditScrollback => self.launch_focused_scrollback_editor(),
            NavigateAction::CopyMode => self.state.enter_copy_mode(&self.terminal_runtimes),
            NavigateAction::Zoom => {
                self.zoom_focused_pane_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::TogglePinTab => {
                self.toggle_pin_active_tab_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::EnterResizeMode => self.state.set_server_mode(Mode::Resize),
            NavigateAction::ResizePaneLeft => {
                self.resize_pane_direction_via_api(NavDirection::Left);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ResizePaneDown => {
                self.resize_pane_direction_via_api(NavDirection::Down);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ResizePaneUp => {
                self.resize_pane_direction_via_api(NavDirection::Up);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ResizePaneRight => {
                self.resize_pane_direction_via_api(NavDirection::Right);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ToggleSidebar => {
                self.state.toggle_sidebar_collapsed();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::FocusSidebar => {
                self.state.focus_client_on_sidebar();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::AssignPaneToPod => {
                self.open_sidebar_pod_picker_for_focused_pane();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::FocusOwningRepoGroup => {
                self.state.focus_owning_repo_group();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CycleSidebarGroupMode => {
                self.state.cycle_sidebar_group_mode();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::RefreshSidebar => {
                self.request_sidebar_refresh();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ToggleStatusDetail => {
                self.state.status_bar_expanded = !self.state.status_bar_expanded;
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ToggleDock => {
                self.state.dock_collapsed = !self.state.dock_collapsed;
                // Opening the dock focuses its active tab. Without this the home
                // tab opens unfocused, so its keys do nothing and the selected
                // row never expands, with nothing on screen saying why.
                if self.state.dock_collapsed {
                    self.state.dock_home_focused = false;
                    self.state.dock_editor_focused = false;
                    self.state.dock_diff_focused = false;
                    self.state.dock_files_focused = false;
                    self.state.dock_agents_focused = false;
                    self.state.dock_hosts_focused = false;
                } else {
                    sync_dock_tab_focus(&mut self.state);
                }
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::PreviousDockTab => {
                if let (Some(crate::app::DockSurface::Home), Some(previous)) = (
                    self.state.dock_tab,
                    previous_home_section(self.state.dock_home_section),
                ) {
                    self.state.set_dock_home_section(previous);
                } else if let Some(previous) = self.state.adjacent_dock_tab_index(false) {
                    self.state.select_dock_tab_index(previous);
                }
                sync_dock_tab_focus(&mut self.state);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NextDockTab => {
                if let (Some(crate::app::DockSurface::Home), Some(next)) = (
                    self.state.dock_tab,
                    next_home_section(self.state.dock_home_section),
                ) {
                    self.state.set_dock_home_section(next);
                } else if let Some(next) = self.state.adjacent_dock_tab_index(true) {
                    self.state.select_dock_tab_index(next);
                }
                sync_dock_tab_focus(&mut self.state);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenRepoEditor => {
                self.open_repo_editor();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenDockHome => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Home)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockTerminal => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Terminal)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockFiles => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Files)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockDiff => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Diff)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockPr => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Pr)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockLinear => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Linear)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockMissive => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Missive)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockAgents => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Agents)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockShortcuts => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Shortcuts)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockContext => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Context)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenDockSymphony => {
                if self
                    .state
                    .activate_dock_surface(crate::app::DockSurface::Symphony)
                {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::EditScratchpad => {
                self.open_scratchpad_in_editor();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ShowScratchpad => {
                self.state.show_scratchpad_tab();
                sync_dock_tab_focus(&mut self.state);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ToggleNotepad => {
                self.state.toggle_notepad_focus();
                self.apply_notepad_request();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::TogglePomodoro => {
                self.state.toggle_pomodoro(std::time::Instant::now());
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CyclePaneNext => {
                self.cycle_pane_via_api(false);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CyclePanePrevious => {
                self.cycle_pane_via_api(true);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::LastPane => {
                self.last_pane_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::Help => super::modal::open_keybind_help(&mut self.state),
            NavigateAction::Settings => super::settings::open_settings(&mut self.state),
            NavigateAction::OpenCommandPalette => self.state.open_command_palette(),
            NavigateAction::ReloadConfig => {
                self.runtime_server_reload_config("tui.server.reload_config");
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenNotificationTarget => {
                self.focus_toast_target_via_api();
                if self.state.server_mode() == Mode::Navigate {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenWorkUrl | NavigateAction::OpenWorkLink => {
                self.open_or_copy_work_link(crate::app::state::WorkLinkPickerAction::Open, context)
            }
            NavigateAction::CopyWorkUrl | NavigateAction::CopyWorkLink => {
                self.open_or_copy_work_link(crate::app::state::WorkLinkPickerAction::Copy, context)
            }
            NavigateAction::CopyWorkTicket => self.copy_focused_work_ticket(),
            NavigateAction::CopyWorkPr => self.copy_focused_work_pr(),
            NavigateAction::CopyWorkPreview => self.copy_focused_work_preview(),
            NavigateAction::ToggleTheme => {
                self.toggle_theme();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::GitPull => {
                request_git_action(&mut self.state, crate::app::state::GitAction::Pull);
            }
            NavigateAction::GitCommit => {
                request_git_action(&mut self.state, crate::app::state::GitAction::Commit);
            }
            NavigateAction::GitPush => {
                request_git_action(&mut self.state, crate::app::state::GitAction::Push);
            }
            NavigateAction::GitCreatePr => {
                request_git_action(&mut self.state, crate::app::state::GitAction::CreatePr);
            }
            NavigateAction::OpenSymphony => {
                self.state.toggle_symphony();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenRuns => {
                focus_runs_section(&mut self.state);
            }
            NavigateAction::OpenWorkView => {
                self.toggle_work_view();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenUsageView => {
                self.toggle_usage_view();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenTicketView => {
                self.toggle_ticket_view();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenMissiveView => {
                self.toggle_missive_view();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenInbox => {
                self.state.toggle_inbox();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenHome => {
                self.state.toggle_home();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::Detach => {
                super::modal::request_detach(&mut self.state);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenNavigator => {
                self.state.open_navigator_from(&self.terminal_runtimes)
            }
        }

        finish_action_context(&mut self.state, context, previous_mode);
    }

    pub(crate) fn focus_workspace_idx_via_api(&mut self, ws_idx: usize) {
        self.state.sidebar_selected_remote_agent = None;
        let workspace_id = self.public_workspace_id(ws_idx);
        self.runtime_workspace_focus("tui.workspace.focus", workspace_id);
    }

    pub(crate) fn show_work_link_notice(&mut self, message: &str) {
        self.state.copy_feedback = Some(crate::app::state::CopyFeedback {
            message: message.to_string(),
        });
        self.copy_feedback_deadline =
            Some(std::time::Instant::now() + super::super::COPY_FEEDBACK_DURATION);
    }

    fn open_or_copy_work_link(
        &mut self,
        action: crate::app::state::WorkLinkPickerAction,
        context: ActionContext,
    ) {
        let candidates = focused_work_context(&self.state)
            .map(crate::work_context::work_link_candidates)
            .unwrap_or_default();
        match candidates.as_slice() {
            [] => {
                self.show_work_link_notice("focused pane has no work link");
                leave_navigate_mode(&mut self.state);
            }
            [candidate] => {
                self.perform_work_link_action(action, &candidate.url);
                leave_navigate_mode(&mut self.state);
            }
            _ => {
                self.state.work_link_picker = Some(crate::app::state::WorkLinkPickerState {
                    candidates: candidates.into_iter().take(9).collect(),
                    action,
                    return_mode: if context == ActionContext::Navigate {
                        Mode::Navigate
                    } else {
                        Mode::Terminal
                    },
                });
                self.state.set_server_mode(Mode::WorkLinkPicker);
            }
        }
    }

    fn perform_work_link_action(
        &mut self,
        action: crate::app::state::WorkLinkPickerAction,
        url: &str,
    ) {
        match action {
            crate::app::state::WorkLinkPickerAction::Open => {
                if let Err(error) = crate::platform::open_url(url) {
                    tracing::warn!(%error, %url, "failed to open focused pane work link");
                    self.show_work_link_notice("could not open work link");
                }
            }
            crate::app::state::WorkLinkPickerAction::Copy => {
                if self
                    .event_tx
                    .try_send(crate::events::AppEvent::ClipboardWrite {
                        content: url.as_bytes().to_vec(),
                    })
                    .is_err()
                {
                    tracing::warn!("failed to queue focused pane work link clipboard event");
                    self.show_work_link_notice("could not copy work link");
                }
            }
        }
    }

    pub(crate) fn handle_work_link_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.state.work_link_picker.clone() else {
            self.state.set_server_mode(Mode::Terminal);
            return;
        };
        if key.code == KeyCode::Esc {
            self.state.work_link_picker = None;
            self.state.set_server_mode(picker.return_mode);
            return;
        }
        let Some(index) = (match key.code {
            KeyCode::Char(digit @ '1'..='9') if key.modifiers.is_empty() => {
                Some(usize::from(digit as u8 - b'1'))
            }
            _ => None,
        }) else {
            return;
        };
        let Some(candidate) = picker.candidates.get(index) else {
            return;
        };
        let url = candidate.url.clone();
        let still_live = focused_work_context(&self.state).is_some_and(|context| {
            crate::work_context::work_link_candidates(context)
                .iter()
                .any(|live| live.url == url)
        });
        self.state.work_link_picker = None;
        if !still_live {
            self.show_work_link_notice("work link is stale");
            self.state.set_server_mode(picker.return_mode);
            return;
        }
        self.perform_work_link_action(picker.action, &url);
        leave_navigate_mode(&mut self.state);
    }

    fn copy_focused_work_ticket(&mut self) {
        self.copy_focused_work_value(
            focused_work_context(&self.state)
                .and_then(|context| context.primary_ticket().map(str::to_string)),
            "focused pane has no work ticket",
            "could not copy work ticket",
        );
    }

    fn copy_focused_work_pr(&mut self) {
        self.copy_focused_work_value(
            focused_work_context(&self.state)
                .and_then(|context| context.primary_pr().map(str::to_string)),
            "focused pane has no pull request",
            "could not copy pull request",
        );
    }

    fn copy_focused_work_preview(&mut self) {
        self.copy_focused_work_value(
            focused_work_context(&self.state)
                .and_then(|context| context.preview_urls.first().cloned()),
            "focused pane has no preview URL",
            "could not copy preview URL",
        );
    }

    fn copy_focused_work_value(
        &mut self,
        value: Option<String>,
        missing_notice: &str,
        failure_notice: &str,
    ) {
        let Some(value) = value else {
            self.show_work_link_notice(missing_notice);
            leave_navigate_mode(&mut self.state);
            return;
        };
        if self
            .event_tx
            .try_send(crate::events::AppEvent::ClipboardWrite {
                content: value.into_bytes(),
            })
            .is_err()
        {
            tracing::warn!("failed to queue focused pane work-context clipboard event");
            self.show_work_link_notice(failure_notice);
        }
        leave_navigate_mode(&mut self.state);
    }

    pub(crate) fn close_workspace_idx_with_group_via_api(&mut self, ws_idx: usize) {
        let workspace_id = self.public_workspace_id(ws_idx);
        self.runtime_workspace_close_group("tui.workspace.close", workspace_id);
    }

    pub(crate) fn move_workspace_via_api(&mut self, source_ws_idx: usize, insert_idx: usize) {
        let workspace_id = self.public_workspace_id(source_ws_idx);
        self.runtime_workspace_move(
            "tui.workspace.move",
            crate::api::schema::WorkspaceMoveParams {
                workspace_id,
                insert_index: insert_idx,
            },
        );
    }

    pub(crate) fn move_workspace_block_via_api(
        &mut self,
        params: crate::api::schema::WorkspaceMoveBlockParams,
    ) {
        self.runtime_workspace_move_block("tui.workspace.move_block", params);
    }

    pub(crate) fn focus_tab_idx_via_api(&mut self, tab_idx: usize) {
        self.state.sidebar_selected_remote_agent = None;
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_focus("tui.tab.focus", tab_id);
    }

    /// Windows are Herdr tabs. Cycle bursts follow the sidebar's visible order,
    /// frozen briefly so sorting changes caused by focus cannot reshuffle them.
    fn focus_relative_window(&mut self, forward: bool) {
        let windows = self.window_cycle_order_at(Instant::now());
        let Some(next) = relative_window_target_index(&self.state, &windows, forward) else {
            return;
        };
        match windows[next].clone() {
            WindowCycleTarget::Local { ws_idx, tab_idx } => {
                let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
                    return;
                };
                self.state.sidebar_selected_remote_agent = None;
                self.runtime_tab_focus("tui.window.focus_relative", tab_id);
            }
            WindowCycleTarget::Remote(agent_ref) => {
                self.open_fleet_host_from_input(&agent_ref.host, Some(&agent_ref.agent));
                self.state.select_remote_agent_row(agent_ref);
            }
        }
    }

    fn focus_next_blocked_window(&mut self) {
        let Some(target) = next_blocked_window_target(&self.state) else {
            return;
        };
        match target {
            BlockedPaneTarget::Local {
                ws_idx, pane_id, ..
            } => {
                self.state.sidebar_selected_remote_agent = None;
                self.focus_pane_internal_via_api(ws_idx, pane_id);
            }
            BlockedPaneTarget::Remote(agent_ref) => {
                let host_reachable = self
                    .state
                    .fleet_snapshot
                    .hosts
                    .iter()
                    .any(|host| host.name == agent_ref.host && host.reachable);
                if host_reachable {
                    self.open_fleet_host_from_input(&agent_ref.host, Some(&agent_ref.agent));
                }
                self.state.select_remote_agent_row(agent_ref);
            }
        }
    }

    pub(crate) fn close_active_tab_via_api(&mut self) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .map(|workspace| workspace.active_tab_index())
        else {
            return;
        };
        self.close_tab_at_via_api(ws_idx, tab_idx);
    }

    pub(crate) fn close_tab_at_via_api(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(workspace) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        if tab_idx >= workspace.tabs.len() {
            return;
        }
        let tab = &workspace.tabs[tab_idx];
        if tab.layout.pane_count() > 1 {
            // A window close targets its selected pane. Closing the tab would
            // also terminate every sibling agent in that window.
            let selected_pane = tab.layout.focused();
            if let Some(pane_id) = self.public_pane_id(ws_idx, selected_pane) {
                self.runtime_pane_close("tui.pane.close", pane_id);
            }
            return;
        }
        if workspace.tabs.len() == 1 {
            self.state.selected = ws_idx;
            if self.state.confirm_close {
                super::modal::open_confirm_close(&mut self.state);
            } else {
                self.close_workspace_idx_with_group_via_api(ws_idx);
            }
            return;
        }
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_close("tui.tab.close", tab_id);
    }

    pub(crate) fn move_tab_via_api(
        &mut self,
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: usize,
    ) {
        let Some(tab_id) = self.public_tab_id(ws_idx, source_tab_idx) else {
            return;
        };
        self.runtime_tab_move(
            "tui.tab.move",
            crate::api::schema::TabMoveParams {
                tab_id,
                insert_index: insert_idx,
            },
        );
    }

    pub(crate) fn focus_pane_internal_via_api(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) {
        self.state.sidebar_selected_remote_agent = None;
        let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        self.runtime_pane_focus("tui.pane.focus", pane_id);
    }

    pub(crate) fn focus_pane_direction_via_api(&mut self, direction: NavDirection) {
        if let Some((ws_idx, target)) = self.directional_pane_target_from_view(direction) {
            self.focus_pane_internal_via_api(ws_idx, target);
            return;
        }
        self.runtime_pane_focus_direction(
            "tui.pane.focus_direction",
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: None,
                direction: api_pane_direction(direction),
            },
        );
    }

    fn focus_pane_direction_in_context(&mut self, direction: NavDirection, context: ActionContext) {
        let preserve_navigate_mode =
            context == ActionContext::Navigate && self.state.server_mode() == Mode::Navigate;
        self.focus_pane_direction_via_api(direction);
        if preserve_navigate_mode {
            self.state.set_server_mode(Mode::Navigate);
        }
    }

    pub(crate) fn resize_pane_direction_via_api(&mut self, direction: NavDirection) {
        self.runtime_pane_resize(
            "tui.pane.resize",
            crate::api::schema::PaneResizeParams {
                pane_id: None,
                direction: api_pane_direction(direction),
                amount: None,
            },
        );
    }

    pub(crate) fn swap_pane_direction_via_api(&mut self, direction: NavDirection) {
        if let Some((ws_idx, source, target)) = self.directional_pane_swap_from_view(direction) {
            let source_pane_id = self.public_pane_id(ws_idx, source);
            let target_pane_id = self.public_pane_id(ws_idx, target);
            if let (Some(source_pane_id), Some(target_pane_id)) = (source_pane_id, target_pane_id) {
                self.runtime_pane_swap(
                    "tui.pane.swap_exact",
                    crate::api::schema::PaneSwapParams {
                        pane_id: None,
                        direction: None,
                        source_pane_id: Some(source_pane_id),
                        target_pane_id: Some(target_pane_id),
                    },
                );
                return;
            }
        }
        self.runtime_pane_swap(
            "tui.pane.swap",
            crate::api::schema::PaneSwapParams {
                pane_id: None,
                direction: Some(api_pane_direction(direction)),
                source_pane_id: None,
                target_pane_id: None,
            },
        );
    }

    pub(crate) fn split_focused_pane_via_api(
        &mut self,
        direction: crate::api::schema::SplitDirection,
    ) {
        self.runtime_pane_split(
            "tui.pane.split",
            crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction,
                ratio: None,
                cwd: None,
                focus: true,
                right_click: Default::default(),
                env: Default::default(),
                work_context: None,
            },
        );
    }

    /// Drain a tab-row pane-toggle click. A sibling pane in that direction is
    /// closed, otherwise the focused pane splits that way. Both branches go
    /// through the same runtime calls as the split and close keybindings.
    pub(crate) fn apply_pane_toggle_request(&mut self) -> bool {
        let Some(direction) = self.state.request_pane_toggle.take() else {
            return false;
        };
        match self.state.pane_toggle_sibling(direction) {
            Some(pane_id) => {
                if let Some(public_id) = self
                    .state
                    .active
                    .and_then(|ws_idx| self.public_pane_id(ws_idx, pane_id))
                {
                    self.runtime_pane_close("tui.pane.close", public_id);
                }
            }
            None => self.split_focused_pane_via_api(direction.split()),
        }
        true
    }

    pub(crate) fn close_focused_pane_via_api_requires_confirmation(&mut self) -> bool {
        if let Some(agent_ref) = self.state.sidebar_selected_remote_agent.clone() {
            if self.state.confirm_close {
                self.state.confirm_close_workspace_id = None;
                self.state.confirm_close_remote_agent_ref = Some(agent_ref);
                self.state
                    .open_client_overlay(crate::app::state::ClientOverlay::ConfirmClose);
                return true;
            }
            if let Err(error) = self.remote_pane_close(agent_ref.clone()) {
                self.show_remote_pane_lifecycle_error(&agent_ref, error);
            }
            return false;
        }
        let Some((ws_idx, pane_id)) = self.focused_pane_target() else {
            return false;
        };
        let remote_agent = self.fleet_attach_agents.get(&pane_id).cloned().or_else(|| {
            self.remote_focus_operations
                .agent_ref_for_proxy_pane(pane_id)
                .cloned()
        });
        if let Some(agent_ref) = remote_agent {
            if self.state.confirm_close {
                self.state.confirm_close_workspace_id = None;
                self.state.confirm_close_remote_agent_ref = Some(agent_ref);
                self.state
                    .open_client_overlay(crate::app::state::ClientOverlay::ConfirmClose);
            } else if let Err(error) = self.remote_pane_close(agent_ref.clone()) {
                self.show_remote_pane_lifecycle_error(&agent_ref, error);
            }
            return self.state.client_overlay == crate::app::state::ClientOverlay::ConfirmClose;
        }
        let closes_workspace = self.state.close_pane_would_close_workspace(ws_idx, pane_id);
        let closes_worktree_group =
            closes_workspace && self.state.workspace_close_indices(ws_idx).len() > 1;
        if closes_workspace
            && (self.state.confirm_close || closes_worktree_group)
            && self.state.begin_workspace_close_confirmation(ws_idx)
        {
            return true;
        }
        let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return false;
        };
        self.runtime_pane_close("tui.pane.close", pane_id);
        self.state.client_overlay == crate::app::state::ClientOverlay::ConfirmClose
    }

    pub(crate) fn zoom_focused_pane_via_api(&mut self) {
        self.runtime_pane_zoom(
            "tui.pane.zoom",
            crate::api::schema::PaneZoomParams {
                pane_id: None,
                mode: crate::api::schema::PaneZoomMode::Toggle,
            },
        );
    }

    /// Pin or unpin whichever tab is in front. Takes an explicit index so the
    /// same path serves both the keybinding (active tab) and a click on some
    /// other tab's pin glyph.
    pub(crate) fn toggle_pin_tab_via_api(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_pin(
            "tui.tab.pin",
            crate::api::schema::TabPinParams {
                tab_id,
                mode: crate::api::schema::TabPinMode::Toggle,
            },
        );
    }

    /// Toggle the session star through the runtime so the flag is a server fact
    /// every client sees, not TUI-local view state.
    pub(crate) fn toggle_tab_star_via_api(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_star(
            "tui.tab.star",
            crate::api::schema::TabStarParams {
                tab_id,
                mode: crate::api::schema::TabStarMode::Toggle,
            },
        );
    }

    pub(crate) fn toggle_pin_active_tab_via_api(&mut self) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let tab_idx = self.state.workspaces[ws_idx].active_tab;
        self.toggle_pin_tab_via_api(ws_idx, tab_idx);
    }

    pub(crate) fn set_split_ratio_via_api(&mut self, path: Vec<bool>, ratio: f32) {
        self.runtime_layout_set_split_ratio(
            "tui.layout.set_split_ratio",
            crate::api::schema::LayoutSetSplitRatioParams {
                tab_id: None,
                pane_id: None,
                path,
                ratio,
            },
        );
    }

    pub(crate) fn cycle_pane_via_api(&mut self, reverse: bool) {
        let remote_agents = self
            .state
            .remote_agent_panel_entries
            .iter()
            .map(|remote| remote.agent_ref.clone())
            .collect::<Vec<_>>();

        if let Some(selected) = self.state.sidebar_selected_remote_agent.as_ref() {
            if let Some(current) = remote_agents.iter().position(|agent| agent == selected) {
                let next = if reverse {
                    current.checked_sub(1)
                } else {
                    (current + 1 < remote_agents.len()).then_some(current + 1)
                };
                if let Some(next) = next {
                    let target = remote_agents[next].clone();
                    self.state.select_remote_agent_row(target.clone());
                    self.open_fleet_host_from_input(&target.host, Some(&target.agent));
                    return;
                }
                self.state.sidebar_selected_remote_agent = None;
                if let Some((ws_idx, pane_id)) = self.cycle_pane_boundary_target(reverse) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                }
                return;
            }
        }

        let Some((ws_idx, pane_id)) = self.focused_pane_target() else {
            return;
        };
        let Some(tab) = self.state.workspaces[ws_idx].active_tab() else {
            return;
        };
        let ids = tab.layout.pane_ids();
        let Some(pos) = ids.iter().position(|id| *id == pane_id) else {
            return;
        };
        let target = if reverse {
            ids[(pos + ids.len() - 1) % ids.len()]
        } else {
            ids[(pos + 1) % ids.len()]
        };
        let wraps = if reverse {
            pos == 0
        } else {
            pos + 1 == ids.len()
        };
        if wraps && !remote_agents.is_empty() {
            let target = if reverse {
                remote_agents.last().cloned()
            } else {
                remote_agents.first().cloned()
            };
            if let Some(target) = target {
                self.state.select_remote_agent_row(target.clone());
                self.open_fleet_host_from_input(&target.host, Some(&target.agent));
                return;
            }
        }
        self.focus_pane_internal_via_api(ws_idx, target);
    }

    fn cycle_pane_boundary_target(&self, reverse: bool) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let tab = self.state.workspaces.get(ws_idx)?.active_tab()?;
        let ids = tab.layout.pane_ids();
        let pane_id = if reverse { ids.last()? } else { ids.first()? };
        Some((ws_idx, *pane_id))
    }

    pub(crate) fn last_pane_via_api(&mut self) {
        let Some(target) = self.state.previous_pane_focus.clone() else {
            return;
        };
        let Some((ws_idx, _tab_idx)) = self.state.pane_focus_target_indices(&target) else {
            self.state.previous_pane_focus = None;
            return;
        };
        if self.state.current_pane_focus_target().as_ref() == Some(&target) {
            self.state.previous_pane_focus = None;
            return;
        }
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
    }

    pub(crate) fn focus_toast_target_via_api(&mut self) {
        let Some(target) = self
            .state
            .toast
            .as_ref()
            .and_then(|toast| toast.target.clone())
        else {
            return;
        };
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
        self.state.toast = None;
        self.focus_client_on_pane();
    }

    pub(crate) fn focused_pane_target(&self) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        Some((ws_idx, pane_id))
    }

    fn directional_pane_target_from_view(
        &self,
        direction: NavDirection,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let focused = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.is_focused)?;
        let target =
            crate::layout::find_in_direction(focused, direction, &self.state.view.pane_infos)?;
        Some((ws_idx, target))
    }

    fn directional_pane_swap_from_view(
        &self,
        direction: NavDirection,
    ) -> Option<(usize, crate::layout::PaneId, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let focused = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.is_focused)?;
        let target =
            crate::layout::find_in_direction(focused, direction, &self.state.view.pane_infos)?;
        Some((ws_idx, focused.id, target))
    }

    fn relative_visible_workspace(&self, delta: isize) -> Option<usize> {
        let order = self.state.workspace_navigation_order();
        if order.is_empty() {
            return None;
        }
        let current = self.state.active.unwrap_or(self.state.selected);
        let current_pos = order.iter().position(|idx| *idx == current).unwrap_or(0);
        let next = (current_pos as isize + delta).rem_euclid(order.len() as isize) as usize;
        order.get(next).copied()
    }

    fn active_tab_move(&self, delta: isize) -> Option<(usize, usize, usize)> {
        let ws_idx = self.state.active?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let source = ws.active_tab;
        let insert = tab_move_insert_index(ws.tabs.len(), source, delta)?;
        Some((ws_idx, source, insert))
    }

    fn relative_tab(&self, delta: isize) -> Option<usize> {
        let ws = self
            .state
            .active
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))?;
        if ws.tabs.is_empty() {
            return None;
        }
        Some((ws.active_tab as isize + delta).rem_euclid(ws.tabs.len() as isize) as usize)
    }

    fn agent_entry_target(&self, idx: usize) -> Option<(usize, crate::layout::PaneId)> {
        let entries = crate::ui::agent_panel_entries(&self.state);
        let target = entries.get(idx)?;
        target
            .local_target()
            .map(|target| (target.ws_idx, target.pane_id))
    }

    fn relative_agent_entry(&self, forward: bool) -> Option<(usize, crate::ui::AgentPanelEntry)> {
        crate::ui::relative_agent_navigation_entry(&self.state, forward)
    }

    fn focus_agent_panel_entry(&mut self, entry: crate::ui::AgentPanelEntry) {
        if let Some(agent_ref) = entry
            .remote_entry
            .as_ref()
            .map(|remote| remote.agent_ref.clone())
        {
            self.state.select_remote_agent_row(agent_ref.clone());
            self.open_fleet_host_from_input(&agent_ref.host, Some(&agent_ref.agent));
        } else if let Some(target) = entry.local_target() {
            self.state.sidebar_selected_remote_agent = None;
            self.focus_pane_internal_via_api(target.ws_idx, target.pane_id);
            self.state
                .ensure_agent_row_visible(target.ws_idx, target.pane_id);
        }
    }

    fn pass_through_key_to_focused_pane(&mut self, key: TerminalKey) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.focused_pane_id())
        else {
            return false;
        };
        let Some(rt) = self
            .state
            .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
        else {
            return false;
        };

        let bytes = rt.encode_terminal_key(key.clone());
        if bytes.is_empty() || rt.try_send_bytes(Bytes::from(bytes)).is_err() {
            return false;
        }

        self.note_human_key(pane_id, &key);
        self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
        self.state.set_server_mode(Mode::Terminal);
        true
    }

    pub(crate) fn launch_custom_command(
        &mut self,
        binding: crate::config::CustomCommandKeybind,
        context: ActionContext,
    ) {
        let previous_mode = self.state.server_mode();
        let previous_toast = self.state.toast.clone();
        let result = match binding.action {
            crate::config::CustomCommandAction::Shell => self.spawn_custom_command(&binding),
            crate::config::CustomCommandAction::Pane => {
                self.spawn_pane_command(&binding.command, Vec::new())
            }
            crate::config::CustomCommandAction::Popup => self.spawn_custom_popup_command(&binding),
            crate::config::CustomCommandAction::PluginAction => self
                .invoke_plugin_action_from_keybind(binding.command.clone())
                .map_err(std::io::Error::other),
        };
        match result {
            Ok(()) => finish_custom_command_context(&mut self.state, context, previous_mode),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "custom command failed".to_string(),
                    context: err.to_string(),
                    position: None,
                    target: None,
                });
                self.sync_toast_deadline(previous_toast);
                finish_custom_command_context(&mut self.state, context, previous_mode);
            }
        }
    }

    fn spawn_custom_popup_command(
        &mut self,
        binding: &crate::config::CustomCommandKeybind,
    ) -> io::Result<()> {
        self.spawn_popup_shell_command(
            &binding.command,
            None,
            self.custom_command_env().0,
            crate::app::popup::PopupGeometry {
                width: binding.width,
                height: binding.height,
            },
        )
    }

    pub(crate) fn custom_command_env(&self) -> (Vec<(String, String)>, Option<std::path::PathBuf>) {
        let mut env = vec![(
            crate::api::SOCKET_PATH_ENV_VAR.to_string(),
            crate::api::socket_path().display().to_string(),
        )];
        if let Ok(current_exe) = std::env::current_exe() {
            env.push((
                "HERDR_BIN_PATH".to_string(),
                current_exe.display().to_string(),
            ));
        }

        let mut cwd = None;
        if let Some(ws_idx) = self.state.active {
            env.push((
                "HERDR_ACTIVE_WORKSPACE_ID".to_string(),
                self.public_workspace_id(ws_idx),
            ));
            if let Some(workspace) = self.state.workspaces.get(ws_idx) {
                let tab_idx = workspace.active_tab_index();
                if let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) {
                    env.push(("HERDR_ACTIVE_TAB_ID".to_string(), tab_id));
                }
                if let Some(pane_id) = workspace.focused_pane_id() {
                    if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                        env.push(("HERDR_ACTIVE_PANE_ID".to_string(), public_pane_id));
                    }
                    if let Some(pane_cwd) = workspace.active_tab().and_then(|tab| {
                        tab.cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                    }) {
                        env.push((
                            "HERDR_ACTIVE_PANE_CWD".to_string(),
                            pane_cwd.display().to_string(),
                        ));
                        if pane_cwd.is_dir() {
                            cwd = Some(pane_cwd);
                        }
                    }
                }
            }
        }
        (env, cwd)
    }

    fn spawn_custom_command(
        &mut self,
        binding: &crate::config::CustomCommandKeybind,
    ) -> std::io::Result<()> {
        let mut command = crate::platform::detached_custom_command_process(&binding.command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (env, cwd) = self.custom_command_env();
        command.envs(env);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command.spawn()?;
        self.detached_custom_command_children.push(child);
        Ok(())
    }

    pub(super) fn launch_focused_scrollback_editor(&mut self) {
        let previous_toast = self.state.toast.clone();
        match self.open_focused_scrollback_in_editor() {
            Ok(()) => self.sync_toast_deadline(previous_toast),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "edit scrollback failed".to_string(),
                    context: err.to_string(),
                    position: None,
                    target: None,
                });
                self.sync_toast_deadline(previous_toast);
            }
        }
    }

    /// Open the repository scratchpad beside the focused pane using its normal
    /// managed terminal lifecycle. The persistent scratchpad is never a temp file.
    pub(crate) fn open_scratchpad_in_editor(&mut self) {
        let Some(root) = crate::scratchpad::focused_repo_root(&self.state) else {
            self.show_work_link_notice("no repository for this pane");
            return;
        };
        let path = crate::scratchpad::scratchpad_path(&root);
        if let Err(error) = crate::scratchpad::ensure_scratchpad_file(&path) {
            tracing::warn!(path = %path.display(), %error, "could not create scratchpad");
            self.show_work_link_notice("could not create the scratchpad");
            return;
        }
        let (env, cwd) = self.custom_command_env();
        let cwd = cwd.or(Some(root));
        let mut last_error = None;
        for argv in crate::scratchpad::editor_argv_candidates(Some(&path)) {
            match self.spawn_overlay_argv_command(
                &argv,
                cwd.clone(),
                env.clone(),
                Vec::new(),
                false,
            ) {
                Ok((_, new_pane)) => {
                    let terminal_id = new_pane.terminal.id.clone();
                    self.terminal_runtimes
                        .insert(terminal_id.clone(), new_pane.runtime);
                    self.state
                        .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
                    self.state.terminals.insert(terminal_id, new_pane.terminal);
                    self.state.dock_home_focused = false;
                    self.render_dirty.request_generic();
                    self.render_notify.notify_one();
                    return;
                }
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(error) = last_error {
            tracing::warn!(%error, "could not spawn scratchpad editor");
            self.show_work_link_notice("could not open the scratchpad editor");
        } else {
            self.show_work_link_notice("no editor command found");
        }
    }

    fn open_focused_scrollback_in_editor(&mut self) -> std::io::Result<()> {
        let ws_idx = self
            .state
            .active
            .ok_or_else(|| std::io::Error::other("no active workspace"))?;
        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let pane_id = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let scrollback = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .ok_or_else(|| std::io::Error::other("focused pane has no scrollback runtime"))?
            .recent_unwrapped_text_snapshot(usize::MAX)
            .text;

        let path = write_scrollback_temp_file(&scrollback)?;

        let argv = match crate::platform::scrollback_editor_argv(&path) {
            Ok(argv) => argv,
            Err(err) => {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
        };
        let (env, _) = self.custom_command_env();
        let new_pane =
            match self.spawn_overlay_argv_command(&argv, None, env, vec![path.clone()], true) {
                Ok((_, new_pane)) => new_pane,
                Err(err) => {
                    let _ = fs::remove_file(&path);
                    return Err(err);
                }
            };
        let terminal_id = new_pane.terminal.id.clone();
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state.terminals.insert(terminal_id, new_pane.terminal);

        if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: crate::app::state::ToastKind::Finished,
                title: "opened scrollback".to_string(),
                context: format!("focused pane {public_pane_id}"),
                position: None,
                target: None,
            });
        }
        Ok(())
    }

    fn spawn_pane_command(
        &mut self,
        command: &str,
        temp_files: Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        let Some(ws_idx) = self.state.active else {
            return Err(std::io::Error::other("no active workspace"));
        };
        let previous_focus_target = self.state.current_pane_focus_target();
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);
        let (env, _) = self.custom_command_env();
        let pane_terminal_theme = self.state.pane_terminal_theme();
        let pane_terminal_appearance = Some(self.state.pane_terminal_appearance());

        let ws = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let tab_idx = ws.active_tab_index();
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
        let cwd = ws.active_tab().and_then(|tab| {
            tab.cwd_for_pane(
                previous_focus,
                &self.state.terminals,
                &self.terminal_runtimes,
            )
        });
        let new_pane = ws.split_focused_command(
            Direction::Horizontal,
            new_rows,
            new_cols,
            cwd,
            command,
            env,
            self.state.pane_scrollback_limit_bytes,
            pane_terminal_theme,
            pane_terminal_appearance,
        )?;
        let new_pane_id = new_pane.pane_id;
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id: new_pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .layout
            .focus_pane(new_pane_id);
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .zoomed = true;
        self.overlay_panes.insert(
            new_pane_id,
            super::super::OverlayPaneState {
                ws_idx,
                tab_idx,
                previous_focus,
                previous_zoomed,
                temp_files,
            },
        );
        self.state.remove_alias_shadowed_by_new_pane(new_pane_id);
        self.state.set_server_mode(Mode::Terminal);
        Ok(())
    }

    pub(crate) fn spawn_overlay_argv_command(
        &mut self,
        argv: &[String],
        cwd: Option<std::path::PathBuf>,
        extra_env: Vec<(String, String)>,
        temp_files: Vec<std::path::PathBuf>,
        zoom: bool,
    ) -> std::io::Result<(usize, crate::workspace::NewPane)> {
        let Some(ws_idx) = self.state.active else {
            return Err(std::io::Error::other("no active workspace"));
        };
        let previous_focus_target = self.state.current_pane_focus_target();
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);

        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let cwd = cwd.or_else(|| {
            ws.active_tab().and_then(|tab| {
                tab.cwd_for_pane(
                    previous_focus,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            })
        });

        let pane_terminal_theme = self.state.pane_terminal_theme();
        let pane_terminal_appearance = Some(self.state.pane_terminal_appearance());
        let (tab_idx, new_pane, workspace_id) = {
            let ws = self
                .state
                .workspaces
                .get_mut(ws_idx)
                .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
            let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
            let result = ws.split_pane_argv_command(
                previous_focus,
                Direction::Horizontal,
                new_rows,
                new_cols,
                cwd,
                argv,
                extra_env,
                self.state.pane_scrollback_limit_bytes,
                pane_terminal_theme,
                pane_terminal_appearance,
                true,
            );
            let (tab_idx, new_pane) = match result {
                Some(Ok(result)) => result,
                Some(Err(err)) => return Err(err),
                None => return Err(std::io::Error::other("focused pane disappeared")),
            };
            ws.tabs
                .get_mut(tab_idx)
                .ok_or_else(|| std::io::Error::other("overlay tab disappeared"))?
                .zoomed = zoom;
            self.overlay_panes.insert(
                new_pane.pane_id,
                super::super::OverlayPaneState {
                    ws_idx,
                    tab_idx,
                    previous_focus,
                    previous_zoomed,
                    temp_files,
                },
            );
            (tab_idx, new_pane, ws.id.clone())
        };

        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id,
            pane_id: new_pane.pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.state.set_server_mode(Mode::Terminal);
        Ok((ws_idx, new_pane))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingDispatch {
    Direct,
    Prefix,
}

enum PrefixBindingMatch {
    Action(NavigateAction),
    Command(crate::config::CustomCommandKeybind),
    UserAction(usize),
}

fn prefix_binding_for_key(state: &AppState, key: &TerminalKey) -> Option<PrefixBindingMatch> {
    exact_prefix_binding_for_key(state, key).or_else(|| {
        generated_character_key(key)
            .as_ref()
            .and_then(|generated_key| exact_prefix_binding_for_key(state, generated_key))
    })
}

pub(crate) fn is_window_cycle_key(state: &AppState, key: &TerminalKey) -> bool {
    let action = match state.server_mode() {
        Mode::Prefix => match prefix_binding_for_key(state, key) {
            Some(PrefixBindingMatch::Action(action)) => Some(action),
            _ => None,
        },
        Mode::Navigate => navigate_mode_non_indexed_action_for_key(state, key)
            .or_else(|| navigate_mode_indexed_action_for_key(state, key)),
        _ => None,
    };
    matches!(
        action,
        Some(NavigateAction::PreviousWindow | NavigateAction::NextWindow)
    )
}

fn exact_prefix_binding_for_key(state: &AppState, key: &TerminalKey) -> Option<PrefixBindingMatch> {
    non_indexed_action_for_key(state, key, BindingDispatch::Prefix)
        .map(PrefixBindingMatch::Action)
        .or_else(|| {
            command_for_key(state, key, BindingDispatch::Prefix).map(PrefixBindingMatch::Command)
        })
        .or_else(|| {
            user_action_for_key(state, key, BindingDispatch::Prefix)
                .map(PrefixBindingMatch::UserAction)
        })
        .or_else(|| {
            indexed_navigation_action(state, key, BindingDispatch::Prefix)
                .map(PrefixBindingMatch::Action)
        })
}

fn generated_character_key(key: &TerminalKey) -> Option<TerminalKey> {
    let mut characters = key.generated_text.as_deref()?.chars();
    let character = characters.next()?;
    if character.is_control() || characters.next().is_some() {
        return None;
    }
    Some(TerminalKey::new(
        KeyCode::Char(character),
        crossterm::event::KeyModifiers::empty(),
    ))
}

pub(crate) fn command_for_key(
    state: &AppState,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> Option<crate::config::CustomCommandKeybind> {
    state
        .keybinds
        .custom_commands
        .iter()
        .find(|binding| match dispatch {
            BindingDispatch::Direct => binding.bindings.matches_direct_key(key),
            BindingDispatch::Prefix => binding.bindings.matches_prefix_key(key),
        })
        .cloned()
}

pub(crate) fn user_action_for_key(
    state: &AppState,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> Option<usize> {
    let repo = state.focused_repo_slug();
    state
        .keybinds
        .user_actions
        .iter()
        .enumerate()
        .find(|(_, action)| {
            action.applies_to_repo(repo.as_deref())
                && match dispatch {
                    BindingDispatch::Direct => action.bindings.matches_direct_key(key),
                    BindingDispatch::Prefix => action.bindings.matches_prefix_key(key),
                }
        })
        .map(|(index, _)| index)
}

fn unmodified_digit_for_key(key: &TerminalKey) -> Option<char> {
    ('1'..='9').find(|digit| {
        crate::config::terminal_key_matches_combo(
            key,
            (
                KeyCode::Char(*digit),
                crossterm::event::KeyModifiers::empty(),
            ),
        )
    })
}

#[cfg(test)]
pub(super) fn handle_navigate_reserved_key(state: &mut AppState, key: TerminalKey) -> bool {
    if let Some(c) = unmodified_digit_for_key(&key) {
        let idx = (c as usize) - ('1' as usize);
        if let Some(ws_idx) = state.workspace_at_visible_position(idx) {
            state.switch_workspace(ws_idx);
            leave_navigate_mode(state);
        }
        return true;
    }

    let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
    if modifiers.is_empty() {
        match code {
            KeyCode::Enter => {
                if !state.workspaces.is_empty() {
                    state.switch_workspace(state.selected);
                    leave_navigate_mode(state);
                }
                return true;
            }
            KeyCode::Tab => {
                state.cycle_pane(false);
                return true;
            }
            KeyCode::BackTab => {
                state.cycle_pane(true);
                return true;
            }
            KeyCode::Left => {
                state.navigate_pane(NavDirection::Left);
                return true;
            }
            KeyCode::Right => {
                state.navigate_pane(NavDirection::Right);
                return true;
            }
            _ => {}
        }
    }

    if state
        .keybinds
        .navigate
        .workspace_up
        .matches_direct_key(&key)
    {
        state.move_selected_workspace_by_visible_delta(-1);
        return true;
    }
    if state
        .keybinds
        .navigate
        .workspace_down
        .matches_direct_key(&key)
    {
        state.move_selected_workspace_by_visible_delta(1);
        return true;
    }
    if state.keybinds.navigate.pane_left.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Left);
        return true;
    }
    if state.keybinds.navigate.pane_down.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Down);
        return true;
    }
    if state.keybinds.navigate.pane_up.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Up);
        return true;
    }
    if state.keybinds.navigate.pane_right.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Right);
        return true;
    }

    false
}

fn navigate_reserved_action_for_key(state: &AppState, key: &TerminalKey) -> Option<NavigateAction> {
    if let Some(c) = unmodified_digit_for_key(key) {
        return Some(NavigateAction::SwitchWorkspace(
            (c as usize) - ('1' as usize),
        ));
    }

    let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
    if modifiers.is_empty() {
        match code {
            KeyCode::Enter => {
                return (!state.workspaces.is_empty()).then_some(NavigateAction::SwitchWorkspace(
                    state
                        .visible_workspace_order()
                        .iter()
                        .position(|idx| *idx == state.selected)
                        .unwrap_or(state.selected),
                ));
            }
            KeyCode::Tab => return Some(NavigateAction::CyclePaneNext),
            KeyCode::BackTab => return Some(NavigateAction::CyclePanePrevious),
            KeyCode::Left => return Some(NavigateAction::FocusPaneLeft),
            KeyCode::Right => return Some(NavigateAction::FocusPaneRight),
            _ => {}
        }
    }

    if state.keybinds.navigate.workspace_up.matches_direct_key(key)
        || state
            .keybinds
            .navigate
            .workspace_down
            .matches_direct_key(key)
    {
        return None;
    }
    if state.keybinds.navigate.pane_left.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneLeft);
    }
    if state.keybinds.navigate.pane_down.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneDown);
    }
    if state.keybinds.navigate.pane_up.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneUp);
    }
    if state.keybinds.navigate.pane_right.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneRight);
    }

    None
}

pub(super) fn api_pane_direction(direction: NavDirection) -> crate::api::schema::PaneDirection {
    match direction {
        NavDirection::Left => crate::api::schema::PaneDirection::Left,
        NavDirection::Right => crate::api::schema::PaneDirection::Right,
        NavDirection::Up => crate::api::schema::PaneDirection::Up,
        NavDirection::Down => crate::api::schema::PaneDirection::Down,
    }
}

#[cfg(test)]
pub(crate) fn handle_navigate_key(state: &mut AppState, key: KeyEvent) {
    let mut terminal_runtimes = TerminalRuntimeRegistry::new();
    state.update_dismissed = true;
    let terminal_key = TerminalKey::from(key);

    if state.is_prefix_key(&terminal_key) || key.code == KeyCode::Esc {
        leave_navigate_mode(state);
        return;
    }

    if handle_navigate_reserved_key(state, terminal_key.clone()) {
        return;
    }

    if let Some(action) = navigate_mode_action_for_key(state, terminal_key) {
        execute_navigate_action_in_context(
            state,
            &mut terminal_runtimes,
            action,
            ActionContext::Navigate,
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum WindowCycleTarget {
    Local { ws_idx: usize, tab_idx: usize },
    Remote(crate::api::schema::AgentRef),
}

#[derive(Debug, Clone)]
pub(crate) struct WindowCycleSnapshot {
    order: Vec<WindowCycleTarget>,
    local_roots: std::collections::HashMap<(usize, usize), crate::layout::PaneId>,
    last_used_at: Instant,
}

const WINDOW_CYCLE_SNAPSHOT_IDLE: Duration = Duration::from_millis(1500);

impl App {
    fn window_cycle_order_at(&mut self, now: Instant) -> Vec<WindowCycleTarget> {
        let fresh = self.window_cycle_snapshot.as_ref().is_some_and(|snapshot| {
            now.saturating_duration_since(snapshot.last_used_at) < WINDOW_CYCLE_SNAPSHOT_IDLE
        });
        if !fresh {
            let order = window_navigation_order(&self.state);
            let local_roots = order
                .iter()
                .filter_map(|target| match target {
                    WindowCycleTarget::Local { ws_idx, tab_idx } => self
                        .state
                        .workspaces
                        .get(*ws_idx)
                        .and_then(|workspace| workspace.tabs.get(*tab_idx))
                        .map(|tab| ((*ws_idx, *tab_idx), tab.root_pane)),
                    WindowCycleTarget::Remote(_) => None,
                })
                .collect();
            self.window_cycle_snapshot = Some(WindowCycleSnapshot {
                order,
                local_roots,
                last_used_at: now,
            });
        }
        if let Some(snapshot) = self.window_cycle_snapshot.as_mut() {
            snapshot.last_used_at = now;
        }
        self.window_cycle_snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .order
                    .iter()
                    .filter_map(|target| match target {
                        WindowCycleTarget::Local { ws_idx, tab_idx } => {
                            let root = snapshot.local_roots.get(&(*ws_idx, *tab_idx))?;
                            self.state.workspaces.iter().enumerate().find_map(
                                |(current_ws_idx, workspace)| {
                                    workspace.tabs.iter().enumerate().find_map(
                                        |(current_tab_idx, tab)| {
                                            (tab.root_pane == *root).then_some(
                                                WindowCycleTarget::Local {
                                                    ws_idx: current_ws_idx,
                                                    tab_idx: current_tab_idx,
                                                },
                                            )
                                        },
                                    )
                                },
                            )
                        }
                        WindowCycleTarget::Remote(_) => {
                            window_cycle_target_exists(&self.state, target).then(|| target.clone())
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn invalidate_window_cycle_snapshot(&mut self) {
        self.window_cycle_snapshot = None;
    }
}

/// Local spaces in sidebar order; spaces the list does not name follow in
/// index order.
fn cycle_space_order(state: &AppState) -> Vec<usize> {
    let grouped = state.sidebar_group_mode != crate::app::state::SidebarGroupMode::Spaces;
    let mut seen = std::collections::HashSet::new();
    crate::ui::sidebar::workspace_list_entries_for_mode(state, true, state.sidebar_group_mode)
        .into_iter()
        .filter_map(|entry| match entry {
            crate::ui::sidebar::WorkspaceListEntry::Workspace { ws_idx, .. } => Some(ws_idx),
            _ => None,
        })
        // Grouped members follow their root, as the space tree shows them.
        .flat_map(|ws_idx| {
            std::iter::once(ws_idx).chain(if grouped {
                crate::ui::sidebar::sidebar_space_member_indices(state, ws_idx)
            } else {
                Vec::new()
            })
        })
        .chain(0..state.workspaces.len())
        .filter(|ws_idx| !state.workspaces[*ws_idx].is_fleet && seen.insert(*ws_idx))
        .collect()
}

/// Order for `prefix+n` / `prefix+p`: targets appear in sidebar row order.
/// When `skip_collapsed_cycle` is off, hidden targets follow in canonical order.
fn window_navigation_order(state: &AppState) -> Vec<WindowCycleTarget> {
    let include_fleet =
        state.window_cycle_mode == crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
    let rows = crate::ui::sidebar_rows(state);
    let mut order = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        let target = match row {
            crate::ui::SidebarRow::Tab { entry, .. }
            | crate::ui::SidebarRow::Agent { entry, .. } => {
                entry.local_target().map(|target| WindowCycleTarget::Local {
                    ws_idx: target.ws_idx,
                    tab_idx: target.tab_idx,
                })
            }
            crate::ui::SidebarRow::RemoteAgent { entry, .. } if include_fleet => {
                Some(WindowCycleTarget::Remote(entry.agent_ref.clone()))
            }
            // NeedsYou rows intentionally duplicate targets shown in their
            // normal location; they never establish cycle position.
            _ => None,
        };
        if let Some(target) = target.filter(|target| seen.insert(target.clone())) {
            order.push(target);
        }
    }
    if !state.skip_collapsed_cycle {
        for ws_idx in cycle_space_order(state) {
            for tab_idx in 0..state.workspaces[ws_idx].tabs.len() {
                let target = WindowCycleTarget::Local { ws_idx, tab_idx };
                if seen.insert(target.clone()) {
                    order.push(target);
                }
            }
        }
        if include_fleet {
            for entry in &state.remote_agent_panel_entries {
                let target = WindowCycleTarget::Remote(entry.agent_ref.clone());
                if seen.insert(target.clone()) {
                    order.push(target);
                }
            }
        }
    }
    order
}

fn window_cycle_target_exists(state: &AppState, target: &WindowCycleTarget) -> bool {
    match target {
        WindowCycleTarget::Local { ws_idx, tab_idx } => state
            .workspaces
            .get(*ws_idx)
            .is_some_and(|workspace| !workspace.is_fleet && *tab_idx < workspace.tabs.len()),
        WindowCycleTarget::Remote(agent_ref) => state
            .remote_agent_panel_entries
            .iter()
            .any(|entry| entry.agent_ref == *agent_ref),
    }
}

/// Local-only projection kept for pane/workspace tests and call sites that need
/// a concrete Herdr tab identity.
#[cfg(test)]
pub(crate) fn window_cycle_order(state: &AppState) -> Vec<(usize, usize)> {
    window_navigation_order(state)
        .into_iter()
        .filter_map(|target| match target {
            WindowCycleTarget::Local { ws_idx, tab_idx } => Some((ws_idx, tab_idx)),
            WindowCycleTarget::Remote(_) => None,
        })
        .collect()
}

fn relative_window_target_index(
    state: &AppState,
    windows: &[WindowCycleTarget],
    forward: bool,
) -> Option<usize> {
    let current = state
        .sidebar_selected_remote_agent
        .as_ref()
        .map(|agent| WindowCycleTarget::Remote(agent.clone()))
        .or_else(|| {
            let ws_idx = state.active?;
            let workspace = state.workspaces.get(ws_idx)?;
            (!workspace.is_fleet).then_some(WindowCycleTarget::Local {
                ws_idx,
                tab_idx: workspace.active_tab_index(),
            })
        })
        .and_then(|current| windows.iter().position(|target| *target == current));
    if let Some(current) = current {
        crate::workspace::relative_window_index(windows.len(), current, forward)
    } else if windows.is_empty() {
        None
    } else {
        Some(if forward { 0 } else { windows.len() - 1 })
    }
}

/// Panes `next_blocked_window` visits, in sidebar row order.
///
/// The cycle walks the worklist the operator reads. Panes hidden by the current
/// projection are appended afterward so none become unreachable.
/// The shared attention tier is the rule the worklist uses, so a pane is a stop
/// for both red blockers and yellow questions. A latched gate on a pane
/// whose agent has resumed working is not a stop, and neither is a settled pane.
/// Comparing a tab's aggregate state against `Blocked` instead missed usage
/// limits and unanswered gates, ignored the stale-supervisor projection the
/// sidebar renders, and could not reach a blocked pane inside a tab that rolled
/// up to another state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockedPaneTarget {
    Local {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    },
    Remote(crate::api::schema::AgentRef),
}

fn blocked_pane_cycle_in_order(state: &AppState) -> Vec<(BlockedPaneTarget, bool)> {
    let rows = crate::ui::sidebar_rows(state);
    // A shown row stands for its whole tab: split panes share one row.
    let visible_tabs = rows
        .iter()
        .filter_map(|row| match row {
            crate::ui::SidebarRow::Tab { entry, .. }
            | crate::ui::SidebarRow::Agent { entry, .. } => entry.local_target(),
            _ => None,
        })
        .map(|target| (target.ws_idx, target.tab_idx))
        .collect::<std::collections::HashSet<_>>();
    let mut local = crate::ui::all_agent_panel_entries(state)
        .into_iter()
        .filter_map(|entry| {
            let target = entry.local_target()?;
            if state
                .workspaces
                .get(target.ws_idx)
                .is_none_or(|workspace| workspace.is_fleet)
            {
                return None;
            }
            let needs_attention = state
                .workspaces
                .get(target.ws_idx)
                .and_then(|workspace| workspace.tabs.get(target.tab_idx))
                .and_then(|tab| tab.panes.get(&target.pane_id))
                .and_then(|pane| {
                    state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .map(|terminal| pane.agent_projection(terminal).needs_human_attention())
                })
                .unwrap_or(false);
            Some((
                BlockedPaneTarget::Local {
                    ws_idx: target.ws_idx,
                    tab_idx: target.tab_idx,
                    pane_id: target.pane_id,
                },
                needs_attention,
            ))
        })
        .collect::<Vec<_>>();
    let include_fleet =
        state.window_cycle_mode == crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
    let mut remote = if include_fleet {
        state
            .remote_agent_panel_entries
            .iter()
            .filter(|entry| entry.snoozed_until.is_none())
            .map(|entry| {
                (
                    BlockedPaneTarget::Remote(entry.agent_ref.clone()),
                    crate::ui::sidebar::entry_needs_human_attention(entry),
                )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut panes = Vec::with_capacity(local.len() + remote.len());
    for row in rows {
        match row {
            crate::ui::SidebarRow::Tab { entry, .. } => {
                let Some(entry_target) = entry.local_target() else {
                    continue;
                };
                let mut index = 0;
                while index < local.len() {
                    let same_tab = matches!(
                        local[index].0,
                        BlockedPaneTarget::Local { ws_idx, tab_idx, .. }
                            if (ws_idx, tab_idx)
                                == (entry_target.ws_idx, entry_target.tab_idx)
                    );
                    if same_tab {
                        panes.push(local.remove(index));
                    } else {
                        index += 1;
                    }
                }
            }
            crate::ui::SidebarRow::Agent { entry, .. } => {
                let Some(entry_target) = entry.local_target() else {
                    continue;
                };
                if let Some(index) = local.iter().position(|(target, _)| {
                    matches!(
                        target,
                        BlockedPaneTarget::Local { ws_idx, pane_id, .. }
                            if (*ws_idx, *pane_id)
                                == (entry_target.ws_idx, entry_target.pane_id)
                    )
                }) {
                    panes.push(local.remove(index));
                }
            }
            crate::ui::SidebarRow::RemoteAgent { entry, .. } if include_fleet => {
                if let Some(index) = remote.iter().position(|(target, _)| {
                    matches!(
                        target,
                        BlockedPaneTarget::Remote(agent_ref)
                            if agent_ref == &entry.agent_ref
                    )
                }) {
                    panes.push(remote.remove(index));
                }
            }
            _ => {}
        }
    }
    // Collapsed or filtered rows remain keyboard-reachable when the operator
    // has not enabled skip-collapsed cycling. Skip-collapsed still keeps
    // panes whose tab a shown row stands for.
    panes.extend(local.into_iter().filter(|(target, _)| {
        !state.skip_collapsed_cycle
            || matches!(
                target,
                BlockedPaneTarget::Local { ws_idx, tab_idx, .. }
                    if visible_tabs.contains(&(*ws_idx, *tab_idx))
            )
    }));
    if !state.skip_collapsed_cycle {
        panes.extend(remote);
    }
    panes
}

fn next_blocked_window_target(state: &AppState) -> Option<BlockedPaneTarget> {
    let panes = blocked_pane_cycle_in_order(state);
    if panes.is_empty() {
        return None;
    }
    let active_window = state
        .active
        .and_then(|ws_idx| Some((ws_idx, state.workspaces.get(ws_idx)?.active_tab_index())));
    let focused = state.active.and_then(|ws_idx| {
        state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.focused_pane_id())
            .map(|pane_id| (ws_idx, pane_id))
    });
    let selected_remote = state.sidebar_selected_remote_agent.as_ref();
    let start = selected_remote
        .and_then(|selected| {
            panes.iter().position(|(target, _)| {
                matches!(target, BlockedPaneTarget::Remote(agent_ref) if agent_ref == selected)
            })
        })
        .or_else(|| {
            focused.and_then(|focused| {
                panes.iter().position(|(target, _)| {
                    matches!(
                        target,
                        BlockedPaneTarget::Local { ws_idx, pane_id, .. }
                            if (*ws_idx, *pane_id) == focused
                    )
                })
            })
        })
        .or_else(|| {
            active_window.and_then(|window| {
                panes.iter().rposition(|(target, _)| {
                    matches!(
                        target,
                        BlockedPaneTarget::Local { ws_idx, tab_idx, .. }
                            if (*ws_idx, *tab_idx) == window
                    )
                })
            })
        })
        .map_or(0, |current| current + 1);
    (0..panes.len()).find_map(|offset| {
        let (target, blocked) = &panes[(start + offset) % panes.len()];
        blocked.then(|| target.clone())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigateAction {
    NewWorkspace,
    NewThread,
    NewAgentDock,
    OpenAgentFinder,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    RenameWorkspace,
    CloseWorkspace,
    SwitchWorkspace(usize),
    SwitchTab(usize),
    FocusAgent(usize),
    WorkspacePicker,
    PreviousWorkspace,
    NextWorkspace,
    PreviousAgent,
    NextAgent,
    NextReviewAgent,
    NewTab,
    RenameTab,
    ToggleTabPrio,
    TogglePrioPanel,
    ToggleBlockedFilter,
    PreviousTab,
    NextTab,
    MoveTabPrevious,
    MoveTabNext,
    PreviousWindow,
    NextWindow,
    NextBlockedWindow,
    CloseTab,
    RenamePane,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    SwapPaneLeft,
    SwapPaneDown,
    SwapPaneUp,
    SwapPaneRight,
    SplitVertical,
    SplitHorizontal,
    SplitLeft,
    SplitUp,
    ClosePane,
    EditScrollback,
    CopyMode,
    Zoom,
    TogglePinTab,
    EnterResizeMode,
    ResizePaneLeft,
    ResizePaneDown,
    ResizePaneUp,
    ResizePaneRight,
    ToggleSidebar,
    FocusSidebar,
    AssignPaneToPod,
    FocusOwningRepoGroup,
    CycleSidebarGroupMode,
    RefreshSidebar,
    ToggleStatusDetail,
    ToggleDock,
    PreviousDockTab,
    NextDockTab,
    OpenRepoEditor,
    OpenDockHome,
    OpenDockTerminal,
    OpenDockFiles,
    OpenDockDiff,
    OpenDockPr,
    OpenDockLinear,
    OpenDockMissive,
    OpenDockAgents,
    OpenDockShortcuts,
    OpenDockContext,
    OpenDockSymphony,
    OpenInbox,
    OpenHome,
    EditScratchpad,
    ShowScratchpad,
    ToggleNotepad,
    TogglePomodoro,
    CyclePaneNext,
    CyclePanePrevious,
    LastPane,
    Help,
    Settings,
    ReloadConfig,
    OpenNotificationTarget,
    OpenWorkUrl,
    CopyWorkUrl,
    OpenWorkLink,
    CopyWorkLink,
    CopyWorkTicket,
    CopyWorkPr,
    CopyWorkPreview,
    ToggleTheme,
    GitPull,
    GitCommit,
    GitPush,
    GitCreatePr,
    OpenSymphony,
    OpenRuns,
    OpenWorkView,
    OpenUsageView,
    OpenTicketView,
    OpenMissiveView,
    Detach,
    OpenNavigator,
    OpenCommandPalette,
}

fn focus_runs_section(state: &mut AppState) {
    state.sidebar_collapsed = false;
    state.set_sidebar_group_collapsed(crate::ui::sidebar::RUNS_SECTION_TITLE, false);
    if let Some(index) = crate::ui::sidebar_rows(state).iter().position(|row| {
        matches!(row, crate::ui::SidebarRow::SectionHeader { title, .. } if *title == crate::ui::sidebar::RUNS_SECTION_TITLE)
    }) {
        state.workspace_scroll = index;
    }
    state.set_server_mode(crate::app::Mode::Navigate);
}

fn copy_mode_survives_prefix_action(action: NavigateAction) -> bool {
    matches!(
        action,
        NavigateAction::SwitchWorkspace(_)
            | NavigateAction::SwitchTab(_)
            | NavigateAction::FocusAgent(_)
            | NavigateAction::PreviousWorkspace
            | NavigateAction::NextWorkspace
            | NavigateAction::PreviousAgent
            | NavigateAction::NextAgent
            | NavigateAction::NextReviewAgent
            | NavigateAction::PreviousTab
            | NavigateAction::NextTab
            | NavigateAction::MoveTabPrevious
            | NavigateAction::MoveTabNext
            | NavigateAction::ToggleTabPrio
            | NavigateAction::PreviousWindow
            | NavigateAction::NextWindow
            | NavigateAction::NextBlockedWindow
            | NavigateAction::FocusPaneLeft
            | NavigateAction::FocusPaneDown
            | NavigateAction::FocusPaneUp
            | NavigateAction::FocusPaneRight
            | NavigateAction::CyclePaneNext
            | NavigateAction::CyclePanePrevious
            | NavigateAction::LastPane
            | NavigateAction::OpenNotificationTarget
    )
}

fn next_review_agent_target(state: &AppState) -> Option<(usize, crate::layout::PaneId)> {
    let review_bindings = state
        .dock_home_bindings()
        .into_iter()
        .filter(|binding| binding.role == Some(crate::work_context::PaneWorkRole::Review))
        .collect::<Vec<_>>();
    if review_bindings.is_empty() {
        return None;
    }

    let focused = state.active.and_then(|ws_idx| {
        state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.focused_pane_id())
            .map(|pane_id| (ws_idx, pane_id))
    });
    let next = focused
        .and_then(|focused| {
            review_bindings
                .iter()
                .position(|binding| (binding.ws_idx, binding.pane_id) == focused)
        })
        .map(|index| (index + 1) % review_bindings.len())
        .unwrap_or(0);
    review_bindings
        .get(next)
        .map(|binding| (binding.ws_idx, binding.pane_id))
}

fn sync_dock_tab_focus(state: &mut AppState) {
    state.dock_home_focused = state.dock_tab == Some(crate::app::DockSurface::Home);
    state.dock_editor_focused = state.dock_tab == Some(crate::app::DockSurface::Editor);
    state.dock_diff_focused = state.dock_tab == Some(crate::app::DockSurface::Diff);
    state.dock_files_focused = state.dock_tab == Some(crate::app::DockSurface::Files);
    state.dock_agents_focused = state.dock_tab == Some(crate::app::DockSurface::Agents);
    state.dock_hosts_focused = state.dock_tab == Some(crate::app::DockSurface::Hosts);
    state.dock_pr_focused = state.dock_tab == Some(crate::app::DockSurface::Pr);
    state.dock_linear_focused = state.dock_tab == Some(crate::app::DockSurface::Linear);
    if state.dock_agents_focused {
        state.reconcile_dock_agents_selection();
    }
    if state.dock_hosts_focused {
        state.reconcile_dock_hosts_selection();
    }
    state.dock_chooser_focused = state.dock_tab.is_none();
    if state.dock_editor_focused {
        state.retry_dock_editor();
    }
}

fn indexed_navigation_action(
    state: &AppState,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    let kb = &state.keybinds;
    let actual_modifiers = crate::config::normalize_key_combo((key.code, key.modifiers)).1;

    for exact_modifiers in [true, false] {
        let trigger_matches = |binding: &crate::config::IndexedKeybind| {
            let dispatch_matches = match dispatch {
                BindingDispatch::Direct => binding.trigger.is_direct(),
                BindingDispatch::Prefix => binding.trigger.is_prefix(),
            };
            let expected_modifiers = crate::config::normalize_key_combo(binding.trigger.combo()).1;
            dispatch_matches && (actual_modifiers == expected_modifiers) == exact_modifiers
        };

        for binding in &kb.switch_tab {
            if trigger_matches(binding) {
                if let Some(idx) = binding.matched_index(key) {
                    return Some(NavigateAction::SwitchTab(idx));
                }
            }
        }
        for binding in &kb.switch_workspace {
            if trigger_matches(binding) {
                if let Some(idx) = binding.matched_index(key) {
                    return Some(NavigateAction::SwitchWorkspace(idx));
                }
            }
        }
        for binding in &kb.focus_agent {
            if trigger_matches(binding) {
                if let Some(idx) = binding.matched_index(key) {
                    return Some(NavigateAction::FocusAgent(idx));
                }
            }
        }
    }

    None
}

fn action_matches(
    bindings: &crate::config::ActionKeybinds,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> bool {
    match dispatch {
        BindingDispatch::Direct => bindings.matches_direct_key(key),
        BindingDispatch::Prefix => bindings.matches_prefix_key(key),
    }
}

#[cfg(test)]
fn action_for_key(
    state: &AppState,
    key: TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    non_indexed_action_for_key(state, &key, dispatch)
        .or_else(|| indexed_navigation_action(state, &key, dispatch))
}

#[cfg(test)]
pub(crate) fn action_for_key_for_test(
    state: &AppState,
    key: TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    action_for_key(state, key, dispatch)
}

macro_rules! non_indexed_action_bindings {
    ($kb:expr) => {{
        let kb = $kb;
        [
            (&kb.help, NavigateAction::Help),
            (&kb.settings, NavigateAction::Settings),
            (&kb.command_palette, NavigateAction::OpenCommandPalette),
            (&kb.agent_finder, NavigateAction::OpenAgentFinder),
            (&kb.workspace_picker, NavigateAction::WorkspacePicker),
            (&kb.new_workspace, NavigateAction::NewWorkspace),
            (&kb.new_thread, NavigateAction::NewThread),
            (&kb.new_agent_dock, NavigateAction::NewAgentDock),
            (&kb.new_worktree, NavigateAction::NewWorktree),
            (&kb.open_worktree, NavigateAction::OpenWorktree),
            (&kb.remove_worktree, NavigateAction::RemoveWorktree),
            (&kb.rename_workspace, NavigateAction::RenameWorkspace),
            (&kb.close_workspace, NavigateAction::CloseWorkspace),
            (&kb.previous_workspace, NavigateAction::PreviousWorkspace),
            (&kb.next_workspace, NavigateAction::NextWorkspace),
            (&kb.previous_agent, NavigateAction::PreviousAgent),
            (&kb.next_agent, NavigateAction::NextAgent),
            (&kb.next_review_agent, NavigateAction::NextReviewAgent),
            (&kb.new_tab, NavigateAction::NewTab),
            (&kb.rename_tab, NavigateAction::RenameTab),
            (&kb.toggle_tab_prio, NavigateAction::ToggleTabPrio),
            (&kb.toggle_prio_panel, NavigateAction::TogglePrioPanel),
            (
                &kb.toggle_blocked_filter,
                NavigateAction::ToggleBlockedFilter,
            ),
            (&kb.previous_tab, NavigateAction::PreviousTab),
            (&kb.next_tab, NavigateAction::NextTab),
            (&kb.move_tab_previous, NavigateAction::MoveTabPrevious),
            (&kb.move_tab_next, NavigateAction::MoveTabNext),
            (&kb.previous_window, NavigateAction::PreviousWindow),
            (&kb.next_window, NavigateAction::NextWindow),
            (&kb.next_blocked_window, NavigateAction::NextBlockedWindow),
            (&kb.close_tab, NavigateAction::CloseTab),
            (&kb.rename_pane, NavigateAction::RenamePane),
            (&kb.edit_scrollback, NavigateAction::EditScrollback),
            (&kb.copy_mode, NavigateAction::CopyMode),
            (&kb.focus_pane_left, NavigateAction::FocusPaneLeft),
            (&kb.focus_pane_down, NavigateAction::FocusPaneDown),
            (&kb.focus_pane_up, NavigateAction::FocusPaneUp),
            (&kb.focus_pane_right, NavigateAction::FocusPaneRight),
            (&kb.swap_pane_left, NavigateAction::SwapPaneLeft),
            (&kb.swap_pane_down, NavigateAction::SwapPaneDown),
            (&kb.swap_pane_up, NavigateAction::SwapPaneUp),
            (&kb.swap_pane_right, NavigateAction::SwapPaneRight),
            (&kb.last_pane, NavigateAction::LastPane),
            (&kb.cycle_pane_next, NavigateAction::CyclePaneNext),
            (&kb.cycle_pane_previous, NavigateAction::CyclePanePrevious),
            (&kb.split_vertical, NavigateAction::SplitVertical),
            (&kb.split_horizontal, NavigateAction::SplitHorizontal),
            (&kb.split_left, NavigateAction::SplitLeft),
            (&kb.split_up, NavigateAction::SplitUp),
            (&kb.close_pane, NavigateAction::ClosePane),
            (&kb.zoom, NavigateAction::Zoom),
            (&kb.toggle_pin_tab, NavigateAction::TogglePinTab),
            (&kb.resize_mode, NavigateAction::EnterResizeMode),
            (&kb.resize_pane_left, NavigateAction::ResizePaneLeft),
            (&kb.resize_pane_down, NavigateAction::ResizePaneDown),
            (&kb.resize_pane_up, NavigateAction::ResizePaneUp),
            (&kb.resize_pane_right, NavigateAction::ResizePaneRight),
            (&kb.toggle_sidebar, NavigateAction::ToggleSidebar),
            (&kb.focus_sidebar, NavigateAction::FocusSidebar),
            (&kb.assign_pane_to_pod, NavigateAction::AssignPaneToPod),
            (
                &kb.focus_owning_repo_group,
                NavigateAction::FocusOwningRepoGroup,
            ),
            (
                &kb.sidebar_cycle_group_mode,
                NavigateAction::CycleSidebarGroupMode,
            ),
            (&kb.sidebar_refresh, NavigateAction::RefreshSidebar),
            (&kb.toggle_status_detail, NavigateAction::ToggleStatusDetail),
            (&kb.toggle_dock, NavigateAction::ToggleDock),
            (&kb.previous_dock_tab, NavigateAction::PreviousDockTab),
            (&kb.next_dock_tab, NavigateAction::NextDockTab),
            (&kb.editor_open_repo, NavigateAction::OpenRepoEditor),
            (&kb.dock_home, NavigateAction::OpenDockHome),
            (&kb.dock_terminal, NavigateAction::OpenDockTerminal),
            (&kb.dock_files, NavigateAction::OpenDockFiles),
            (&kb.dock_diff, NavigateAction::OpenDockDiff),
            (&kb.dock_pr, NavigateAction::OpenDockPr),
            (&kb.dock_linear, NavigateAction::OpenDockLinear),
            (&kb.dock_missive, NavigateAction::OpenDockMissive),
            (&kb.dock_agents, NavigateAction::OpenDockAgents),
            (&kb.dock_shortcuts, NavigateAction::OpenDockShortcuts),
            (&kb.dock_context, NavigateAction::OpenDockContext),
            (&kb.dock_symphony, NavigateAction::OpenDockSymphony),
            (&kb.edit_scratchpad, NavigateAction::EditScratchpad),
            (&kb.show_scratchpad, NavigateAction::ShowScratchpad),
            (&kb.toggle_notepad, NavigateAction::ToggleNotepad),
            (&kb.toggle_pomodoro, NavigateAction::TogglePomodoro),
            (&kb.symphony, NavigateAction::OpenSymphony),
            (&kb.runs, NavigateAction::OpenRuns),
            (&kb.work, NavigateAction::OpenWorkView),
            (&kb.usage, NavigateAction::OpenUsageView),
            (&kb.tickets, NavigateAction::OpenTicketView),
            (&kb.missive, NavigateAction::OpenMissiveView),
            (&kb.inbox, NavigateAction::OpenInbox),
            (&kb.home, NavigateAction::OpenHome),
            (&kb.reload_config, NavigateAction::ReloadConfig),
            (
                &kb.open_notification_target,
                NavigateAction::OpenNotificationTarget,
            ),
            (&kb.open_work_url, NavigateAction::OpenWorkUrl),
            (&kb.copy_work_url, NavigateAction::CopyWorkUrl),
            (&kb.open_work_link, NavigateAction::OpenWorkLink),
            (&kb.copy_work_link, NavigateAction::CopyWorkLink),
            (&kb.copy_work_ticket, NavigateAction::CopyWorkTicket),
            (&kb.copy_work_pr, NavigateAction::CopyWorkPr),
            (&kb.copy_work_preview, NavigateAction::CopyWorkPreview),
            (&kb.toggle_theme, NavigateAction::ToggleTheme),
            (&kb.git_pull, NavigateAction::GitPull),
            (&kb.git_commit, NavigateAction::GitCommit),
            (&kb.git_push, NavigateAction::GitPush),
            (&kb.git_create_pr, NavigateAction::GitCreatePr),
            (&kb.detach, NavigateAction::Detach),
            (&kb.goto, NavigateAction::OpenNavigator),
        ]
    }};
}

fn non_indexed_action_for_key(
    state: &AppState,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    let kb = &state.keybinds;
    for (bindings, action) in non_indexed_action_bindings!(kb) {
        if action_matches(bindings, key, dispatch) {
            return Some(action);
        }
    }
    None
}

#[cfg(test)]
pub(crate) fn non_indexed_navigation_actions_for_test(
    keybinds: &crate::config::Keybinds,
) -> Vec<NavigateAction> {
    non_indexed_action_bindings!(keybinds)
        .into_iter()
        .map(|(_, action)| action)
        .collect()
}

#[cfg(test)]
fn navigate_mode_action_for_key(state: &AppState, key: TerminalKey) -> Option<NavigateAction> {
    let action = action_for_key(state, key, BindingDispatch::Prefix)?;
    if matches!(
        action,
        NavigateAction::FocusPaneLeft
            | NavigateAction::FocusPaneDown
            | NavigateAction::FocusPaneUp
            | NavigateAction::FocusPaneRight
    ) {
        return None;
    }
    Some(action)
}

fn navigate_mode_non_indexed_action_for_key(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    let action = non_indexed_action_for_key(state, key, BindingDispatch::Prefix)?;
    if matches!(
        action,
        NavigateAction::FocusPaneLeft
            | NavigateAction::FocusPaneDown
            | NavigateAction::FocusPaneUp
            | NavigateAction::FocusPaneRight
    ) {
        return None;
    }
    Some(action)
}

fn navigate_mode_indexed_action_for_key(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    indexed_navigation_action(state, key, BindingDispatch::Prefix)
}

#[cfg(test)]
pub(super) fn execute_navigate_action(state: &mut AppState, action: NavigateAction) {
    let mut terminal_runtimes = TerminalRuntimeRegistry::new();
    execute_navigate_action_in_context(
        state,
        &mut terminal_runtimes,
        action,
        ActionContext::Navigate,
    );
}

#[cfg(test)]
pub(super) fn execute_navigate_action_in_context(
    state: &mut AppState,
    terminal_runtimes: &mut TerminalRuntimeRegistry,
    action: NavigateAction,
    context: ActionContext,
) {
    let previous_mode = state.server_mode();
    match action {
        NavigateAction::NewWorkspace => {
            state.request_new_workspace = true;
            leave_navigate_mode(state);
        }
        NavigateAction::NewThread => {
            state.focus_client_on_sidebar();
            state.open_sidebar_new_thread();
            leave_navigate_mode(state);
        }
        NavigateAction::NewAgentDock => {
            leave_navigate_mode(state);
            state.open_spawn_dock();
        }
        NavigateAction::OpenAgentFinder => leave_navigate_mode(state),
        NavigateAction::NewWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context)
                .filter(|idx| workspace_can_start_worktree_action(state, terminal_runtimes, *idx))
            {
                state.request_new_linked_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context)
                .filter(|idx| workspace_can_start_worktree_action(state, terminal_runtimes, *idx))
            {
                state.request_open_existing_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RemoveWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                state.request_remove_linked_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RenameWorkspace => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                super::modal::open_rename_workspace(state, terminal_runtimes, ws_idx);
            }
        }
        NavigateAction::CloseWorkspace => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                state.selected = ws_idx;
                if state.confirm_close {
                    super::modal::open_confirm_close(state);
                } else {
                    state.close_selected_workspace();
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::SwitchWorkspace(idx) => {
            if let Some(ws_idx) = state.workspace_at_visible_position(idx) {
                state.switch_workspace(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::SwitchTab(idx) => {
            let tab_exists = state
                .active
                .and_then(|ws_idx| state.workspaces.get(ws_idx))
                .is_some_and(|ws| idx < ws.tabs.len());
            if tab_exists {
                state.switch_tab(idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::FocusAgent(idx) => {
            if state.focus_agent_entry(idx) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::WorkspacePicker => {
            state.mobile_switcher_scroll = 0;
            state.set_server_mode(Mode::Navigate);
        }
        NavigateAction::PreviousWorkspace => {
            state.previous_workspace();
            leave_navigate_mode(state);
        }
        NavigateAction::NextWorkspace => {
            state.next_workspace();
            leave_navigate_mode(state);
        }
        NavigateAction::PreviousAgent => {
            state.previous_agent();
            leave_navigate_mode(state);
        }
        NavigateAction::NextAgent => {
            state.next_agent();
            leave_navigate_mode(state);
        }
        NavigateAction::NextReviewAgent => {
            if let Some((ws_idx, pane_id)) = next_review_agent_target(state) {
                state.focus_pane_in_workspace(ws_idx, pane_id);
                state.dock_home_focused = false;
                state.dock_diff_focused = false;
                state.dock_files_focused = false;
                state.dock_agents_focused = false;
                state.dock_hosts_focused = false;
                state.dock_linear_focused = false;
            }
            leave_navigate_mode(state);
        }
        NavigateAction::NewTab => {
            if state.active.is_some() {
                if state.prompt_new_tab_name {
                    super::modal::open_new_tab_dialog(state);
                } else {
                    state.request_new_tab = true;
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::RenameTab => super::modal::open_rename_active_tab(state, false),
        NavigateAction::ToggleTabPrio => {
            if toggle_tab_prio(state, context) {
                state.mark_session_dirty();
                if context == ActionContext::Navigate {
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::TogglePrioPanel => {
            state.toggle_prio_panel();
            state.mark_session_dirty();
            if context == ActionContext::Navigate {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::ToggleBlockedFilter => {
            state.blocked_filter = !state.blocked_filter;
            state.workspace_scroll = crate::ui::normalized_workspace_scroll(
                state,
                state.view.sidebar_rect,
                state.workspace_scroll,
            );
            if context == ActionContext::Navigate {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::PreviousTab => {
            state.previous_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::NextTab => {
            state.next_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::MoveTabPrevious => {
            move_active_tab_relative(state, -1);
            leave_navigate_mode(state);
        }
        NavigateAction::MoveTabNext => {
            move_active_tab_relative(state, 1);
            leave_navigate_mode(state);
        }
        NavigateAction::PreviousWindow | NavigateAction::NextWindow => {
            let windows = window_navigation_order(state);
            let forward = matches!(action, NavigateAction::NextWindow);
            if let Some(next) = relative_window_target_index(state, &windows, forward) {
                match windows[next].clone() {
                    WindowCycleTarget::Local { ws_idx, tab_idx } => {
                        state.sidebar_selected_remote_agent = None;
                        state.switch_workspace(ws_idx);
                        state.switch_tab(tab_idx);
                    }
                    WindowCycleTarget::Remote(agent_ref) => {
                        state.select_remote_agent_row(agent_ref);
                    }
                }
            }
            leave_navigate_mode(state);
        }
        NavigateAction::NextBlockedWindow => {
            if let Some(target) = next_blocked_window_target(state) {
                match target {
                    BlockedPaneTarget::Local {
                        ws_idx, pane_id, ..
                    } => {
                        state.sidebar_selected_remote_agent = None;
                        state.focus_pane_in_workspace(ws_idx, pane_id);
                    }
                    BlockedPaneTarget::Remote(agent_ref) => {
                        state.select_remote_agent_row(agent_ref);
                    }
                }
            }
            leave_navigate_mode(state);
        }
        NavigateAction::CloseTab => {
            if !state.close_tab() {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RenamePane => {
            if let Some(pane_id) = state
                .active
                .and_then(|ws_idx| state.workspaces.get(ws_idx))
                .and_then(|ws| ws.focused_pane_id())
            {
                super::modal::open_rename_pane(state, pane_id);
            }
        }
        NavigateAction::FocusPaneLeft => state.navigate_pane(NavDirection::Left),
        NavigateAction::FocusPaneDown => state.navigate_pane(NavDirection::Down),
        NavigateAction::FocusPaneUp => state.navigate_pane(NavDirection::Up),
        NavigateAction::FocusPaneRight => state.navigate_pane(NavDirection::Right),
        NavigateAction::SwapPaneLeft => {
            state.swap_pane(NavDirection::Left);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneDown => {
            state.swap_pane(NavDirection::Down);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneUp => {
            state.swap_pane(NavDirection::Up);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneRight => {
            state.swap_pane(NavDirection::Right);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitVertical => {
            state.split_pane(terminal_runtimes, Direction::Horizontal);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitHorizontal => {
            state.split_pane(terminal_runtimes, Direction::Vertical);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitLeft => {
            state.split_pane_with_placement(terminal_runtimes, Direction::Horizontal, true);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitUp => {
            state.split_pane_with_placement(terminal_runtimes, Direction::Vertical, true);
            leave_navigate_mode(state);
        }
        NavigateAction::ClosePane => {
            if !state.close_pane() {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::EditScrollback => {}
        NavigateAction::CopyMode => state.enter_copy_mode(terminal_runtimes),
        NavigateAction::Zoom => {
            state.toggle_zoom();
            leave_navigate_mode(state);
        }
        // Headless/test dispatch has no API client, so it flips the flag it
        // would otherwise have asked the server to flip.
        NavigateAction::TogglePinTab => {
            state.toggle_pin_active_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::EnterResizeMode => state.set_server_mode(Mode::Resize),
        NavigateAction::ResizePaneLeft => {
            state.resize_pane(NavDirection::Left);
            leave_navigate_mode(state);
        }
        NavigateAction::ResizePaneDown => {
            state.resize_pane(NavDirection::Down);
            leave_navigate_mode(state);
        }
        NavigateAction::ResizePaneUp => {
            state.resize_pane(NavDirection::Up);
            leave_navigate_mode(state);
        }
        NavigateAction::ResizePaneRight => {
            state.resize_pane(NavDirection::Right);
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleSidebar => {
            state.toggle_sidebar_collapsed();
            leave_navigate_mode(state);
        }
        NavigateAction::FocusSidebar => {
            state.focus_client_on_sidebar();
            leave_navigate_mode(state);
        }
        NavigateAction::AssignPaneToPod => {
            if let Some((ws_idx, pane_id)) = state.active.and_then(|ws_idx| {
                state
                    .workspaces
                    .get(ws_idx)
                    .and_then(crate::workspace::Workspace::focused_pane_id)
                    .map(|pane_id| (ws_idx, pane_id))
            }) {
                state.sidebar_pod_picker = Some(crate::app::state::SidebarPodPickerState {
                    ws_idx,
                    pane_id,
                    anchor: (state.view.sidebar_rect.x, state.view.sidebar_rect.y),
                    filter: crate::ui::dropdown::DropdownFilterState::default(),
                });
                state.focus_client_on_sidebar();
            }
            leave_navigate_mode(state);
        }
        NavigateAction::FocusOwningRepoGroup => {
            state.focus_owning_repo_group();
            leave_navigate_mode(state);
        }
        NavigateAction::CycleSidebarGroupMode => {
            state.cycle_sidebar_group_mode();
            leave_navigate_mode(state);
        }
        NavigateAction::RefreshSidebar => {
            state.request_sidebar_refresh();
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleStatusDetail => {
            state.status_bar_expanded = !state.status_bar_expanded;
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleDock => {
            state.dock_collapsed = !state.dock_collapsed;
            // Headless mirror of the interactive arm above.
            if state.dock_collapsed {
                state.dock_home_focused = false;
                state.dock_editor_focused = false;
                state.dock_diff_focused = false;
                state.dock_files_focused = false;
                state.dock_agents_focused = false;
                state.dock_hosts_focused = false;
            } else {
                sync_dock_tab_focus(state);
            }
            leave_navigate_mode(state);
        }
        NavigateAction::PreviousDockTab => {
            if let (Some(crate::app::DockSurface::Home), Some(previous)) = (
                state.dock_tab,
                previous_home_section(state.dock_home_section),
            ) {
                state.set_dock_home_section(previous);
            } else if let Some(previous) = state.adjacent_dock_tab_index(false) {
                state.select_dock_tab_index(previous);
            }
            sync_dock_tab_focus(state);
            leave_navigate_mode(state);
        }
        NavigateAction::NextDockTab => {
            if let (Some(crate::app::DockSurface::Home), Some(next)) =
                (state.dock_tab, next_home_section(state.dock_home_section))
            {
                state.set_dock_home_section(next);
            } else if let Some(next) = state.adjacent_dock_tab_index(true) {
                state.select_dock_tab_index(next);
            }
            sync_dock_tab_focus(state);
            leave_navigate_mode(state);
        }
        NavigateAction::OpenRepoEditor => {
            state.request_open_repo_editor = true;
            leave_navigate_mode(state);
        }
        NavigateAction::OpenDockHome => {
            if state.activate_dock_surface(crate::app::DockSurface::Home) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockTerminal => {
            if state.activate_dock_surface(crate::app::DockSurface::Terminal) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockFiles => {
            if state.activate_dock_surface(crate::app::DockSurface::Files) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockDiff => {
            if state.activate_dock_surface(crate::app::DockSurface::Diff) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockPr => {
            if state.activate_dock_surface(crate::app::DockSurface::Pr) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockLinear => {
            if state.activate_dock_surface(crate::app::DockSurface::Linear) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockMissive => {
            if state.activate_dock_surface(crate::app::DockSurface::Missive) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockAgents => {
            if state.activate_dock_surface(crate::app::DockSurface::Agents) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockShortcuts => {
            if state.activate_dock_surface(crate::app::DockSurface::Shortcuts) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockContext => {
            if state.activate_dock_surface(crate::app::DockSurface::Context) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenDockSymphony => {
            if state.activate_dock_surface(crate::app::DockSurface::Symphony) {
                leave_navigate_mode(state);
            }
        }
        // Spawning the editor needs an `App`; the state-only mirror cannot do it.
        NavigateAction::EditScratchpad => leave_navigate_mode(state),
        NavigateAction::ShowScratchpad => {
            state.show_scratchpad_tab();
            sync_dock_tab_focus(state);
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleNotepad => {
            state.toggle_notepad_focus();
            leave_navigate_mode(state);
        }
        NavigateAction::TogglePomodoro => {
            state.toggle_pomodoro(std::time::Instant::now());
            leave_navigate_mode(state);
        }
        NavigateAction::CyclePaneNext => {
            state.cycle_pane(false);
            leave_navigate_mode(state);
        }
        NavigateAction::CyclePanePrevious => {
            state.cycle_pane(true);
            leave_navigate_mode(state);
        }
        NavigateAction::LastPane => {
            state.last_pane();
            leave_navigate_mode(state);
        }
        NavigateAction::Help => super::modal::open_keybind_help(state),
        NavigateAction::Settings => super::settings::open_settings(state),
        NavigateAction::OpenCommandPalette => state.open_command_palette(),
        NavigateAction::ReloadConfig => {
            state.request_reload_config = true;
            leave_navigate_mode(state);
        }
        NavigateAction::OpenNotificationTarget => {
            state.focus_toast_target();
            if state.server_mode() == Mode::Navigate {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenWorkUrl
        | NavigateAction::CopyWorkUrl
        | NavigateAction::OpenWorkLink
        | NavigateAction::CopyWorkLink
        | NavigateAction::CopyWorkTicket
        | NavigateAction::CopyWorkPr
        | NavigateAction::CopyWorkPreview => {
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleTheme => {
            // Theme refresh owns the palette, redraw, and pane propagation on App.
        }
        NavigateAction::GitPull => {
            request_git_action(state, crate::app::state::GitAction::Pull);
        }
        NavigateAction::GitCommit => {
            request_git_action(state, crate::app::state::GitAction::Commit);
        }
        NavigateAction::GitPush => {
            request_git_action(state, crate::app::state::GitAction::Push);
        }
        NavigateAction::GitCreatePr => {
            request_git_action(state, crate::app::state::GitAction::CreatePr);
        }
        NavigateAction::OpenSymphony => {
            state.toggle_symphony();
            leave_navigate_mode(state);
        }
        NavigateAction::OpenRuns => {
            focus_runs_section(state);
        }
        NavigateAction::OpenWorkView => {
            state.work_view = Some(crate::app::state::WorkViewState::new(false, None));
            state.follow_view(crate::app::state::SidebarGroupMode::RepoPr);
            leave_navigate_mode(state);
        }
        NavigateAction::OpenTicketView => {
            let mut view = crate::app::state::WorkViewState::new(false, None);
            view.projection = crate::app::state::WorkProjection::Tickets;
            state.work_view = Some(view);
            state.follow_view(crate::app::state::SidebarGroupMode::LinearTeam);
            leave_navigate_mode(state);
        }
        NavigateAction::OpenMissiveView => {
            let mut view = crate::app::state::WorkViewState::new(false, None);
            view.projection = crate::app::state::WorkProjection::Missive;
            state.work_view = Some(view);
            leave_navigate_mode(state);
        }
        NavigateAction::OpenUsageView => {
            state.toggle_usage_view();
            leave_navigate_mode(state);
        }
        NavigateAction::OpenInbox => {
            state.toggle_inbox();
            leave_navigate_mode(state);
        }
        NavigateAction::OpenHome => {
            state.toggle_home();
            leave_navigate_mode(state);
        }
        NavigateAction::Detach => {
            super::modal::request_detach(state);
            leave_navigate_mode(state);
        }
        NavigateAction::OpenNavigator => state.open_navigator_from(terminal_runtimes),
    }

    finish_action_context(state, context, previous_mode);
}

fn workspace_action_target(state: &AppState, context: ActionContext) -> Option<usize> {
    let idx = match context {
        ActionContext::Direct | ActionContext::Prefix => state.active.unwrap_or(state.selected),
        ActionContext::Navigate => state.selected,
    };
    (idx < state.workspaces.len()).then_some(idx)
}

fn request_git_action(state: &mut AppState, action: crate::app::state::GitAction) -> bool {
    if !crate::ui::dock::chooser::focused_in_git_repo(state) {
        return false;
    }
    state.request_git_action = Some(action);
    state.set_server_mode(Mode::Terminal);
    true
}

fn toggle_tab_prio(state: &mut AppState, context: ActionContext) -> bool {
    let Some(ws_idx) = workspace_action_target(state, context) else {
        return false;
    };
    let Some(tab_idx) = state
        .workspaces
        .get(ws_idx)
        .map(crate::workspace::Workspace::active_tab_index)
    else {
        return false;
    };
    state
        .apply_tab_prio(ws_idx, tab_idx, crate::workspace::TabPrioAction::Toggle)
        .is_some()
}

#[cfg(test)]
fn focused_work_url(state: &AppState) -> Option<String> {
    focused_work_context(state)?.primary_action_url()
}

fn focused_work_context(state: &AppState) -> Option<&crate::work_context::PaneWorkContext> {
    let workspace = state
        .active
        .and_then(|ws_idx| state.workspaces.get(ws_idx))?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal_id = workspace.terminal_id(pane_id)?;
    Some(state.terminals.get(terminal_id)?.effective_work_context())
}

fn workspace_can_start_worktree_action(
    state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    ws_idx: usize,
) -> bool {
    let Some(ws) = state.workspaces.get(ws_idx) else {
        return false;
    };
    if ws
        .worktree_space()
        .is_some_and(|space| space.is_linked_worktree)
    {
        return false;
    }
    let git_space = ws.git_space().cloned().or_else(|| {
        ws.resolved_identity_cwd_from(&state.terminals, terminal_runtimes)
            .as_deref()
            .and_then(crate::workspace::git_space_metadata)
    });
    !git_space.is_some_and(|space| space.is_linked_worktree)
}

// Translate a one-step move into the pre-removal insertion slot that
// Workspace::move_tab expects, wrapping at either end. None when there is
// nothing to move.
fn tab_move_insert_index(len: usize, source: usize, delta: isize) -> Option<usize> {
    if len <= 1 {
        return None;
    }
    Some(if delta > 0 {
        if source + 1 >= len {
            0
        } else {
            source + 2
        }
    } else if source == 0 {
        len
    } else {
        source - 1
    })
}

#[cfg(test)]
fn move_active_tab_relative(state: &mut AppState, delta: isize) {
    let Some(ws) = state
        .active
        .and_then(|ws_idx| state.workspaces.get_mut(ws_idx))
    else {
        return;
    };
    let source = ws.active_tab;
    if let Some(insert) = tab_move_insert_index(ws.tabs.len(), source, delta) {
        ws.move_tab(source, insert);
    }
}

fn leave_navigate_mode(state: &mut AppState) {
    state.end_workspace_picker_presentation();
    if state.active.is_some() {
        state.set_server_mode(Mode::Terminal);
    }
}

fn finish_action_context(state: &mut AppState, context: ActionContext, previous_mode: Mode) {
    if matches!(context, ActionContext::Direct | ActionContext::Prefix)
        && state.server_mode() == previous_mode
    {
        leave_command_mode(state);
    }
}

fn finish_custom_command_context(
    state: &mut AppState,
    context: ActionContext,
    previous_mode: Mode,
) {
    if context == ActionContext::Navigate {
        leave_navigate_mode(state);
    } else {
        finish_action_context(state, context, previous_mode);
    }
}

fn leave_command_mode(state: &mut AppState) {
    if state.copy_mode_pane_is_focused() {
        state.set_server_mode(Mode::Copy);
    } else if state.active.is_some() {
        state.set_server_mode(Mode::Terminal);
    } else {
        state.set_server_mode(Mode::Navigate);
    };
}

fn write_scrollback_temp_file(content: &str) -> io::Result<std::path::PathBuf> {
    let mut last_collision = None;
    for attempt in 0..16 {
        let path = unique_scrollback_path(attempt);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(err);
            }
            Err(err) => return Err(err),
        }
    }

    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique scrollback temp file",
        )
    }))
}

fn unique_scrollback_path(attempt: u32) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "herdr-scrollback-{}-{nanos}-{attempt}.txt",
        std::process::id()
    ))
}

/// The home sections cycle in order; `None` means the dock tab itself should
/// move instead.
fn next_home_section(
    section: crate::app::state::DockHomeSection,
) -> Option<crate::app::state::DockHomeSection> {
    match section {
        crate::app::state::DockHomeSection::Prs => {
            Some(crate::app::state::DockHomeSection::Tickets)
        }
        crate::app::state::DockHomeSection::Tickets => {
            Some(crate::app::state::DockHomeSection::XPolls)
        }
        crate::app::state::DockHomeSection::XPolls => None,
    }
}

fn previous_home_section(
    section: crate::app::state::DockHomeSection,
) -> Option<crate::app::state::DockHomeSection> {
    match section {
        crate::app::state::DockHomeSection::Prs => None,
        crate::app::state::DockHomeSection::Tickets => {
            Some(crate::app::state::DockHomeSection::Prs)
        }
        crate::app::state::DockHomeSection::XPolls => {
            Some(crate::app::state::DockHomeSection::Tickets)
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::Duration;

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::{Direction, Rect};

    use super::super::{state_with_workspaces, unique_temp_path};
    #[cfg(unix)]
    use super::super::{wait_for_custom_command_reap, wait_for_file};
    use super::*;
    use crate::{
        app::App,
        config::Config,
        input::TerminalKey,
        raw_input::{parse_raw_input_bytes_sync, RawInputEvent},
        terminal::TerminalState,
        workspace::Workspace,
    };

    #[test]
    fn freeze_closing_a_window_closes_only_its_selected_agent_pane() {
        let mut app = app_with_test_workspaces(&["agents"]);
        app.state.confirm_close = false;
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let selected = app.state.workspaces[0].test_split(Direction::Horizontal);
        let third = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(selected);
        app.state.ensure_test_terminals();
        let first_terminal = app.state.terminal_id_for_pane(0, first).unwrap().clone();
        let third_terminal = app.state.terminal_id_for_pane(0, third).unwrap().clone();
        app.state
            .terminals
            .get_mut(&first_terminal)
            .unwrap()
            .set_raw_agent_state_for_test(crate::detect::AgentState::Working);
        app.state
            .terminals
            .get_mut(&third_terminal)
            .unwrap()
            .set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);
        app.close_tab_at_via_api(0, 0);
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert!(app.state.workspaces[0].pane_state(selected).is_none());
        assert!(app.state.workspaces[0].pane_state(first).is_some());
        assert!(app.state.workspaces[0].pane_state(third).is_some());
        assert_eq!(
            app.state.terminals[&first_terminal].raw_agent_state(),
            crate::detect::AgentState::Working
        );
        assert_eq!(
            app.state.terminals[&third_terminal].raw_agent_state(),
            crate::detect::AgentState::Blocked
        );
        app.state.assert_invariants_for_test();
    }

    fn mark_worktree_space_member(state: &mut AppState, ws_idx: usize, key: &str) {
        state.workspaces[ws_idx].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/worktree-{ws_idx}").into(),
            is_linked_worktree: ws_idx != 0,
        });
    }

    fn app_with_test_workspaces(names: &[&str]) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        app.state.ensure_test_terminals();
        app.state.active = (!app.state.workspaces.is_empty()).then_some(0);
        app.state.selected = 0;
        app
    }

    #[tokio::test]
    async fn spawn_dock_owns_prefix_keys_and_persists_prompt_when_closed() {
        let mut app = app_with_test_workspaces(&["local"]);
        app.state.sidebar_presentation.spawn_dock_client_id = Some("test-spawn-dock".into());
        app.state.sidebar_presentation.save_spawn_dock_draft(None);
        app.state.active = None;
        app.state.set_server_mode(Mode::Prefix);

        app.handle_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()))
            .await;
        assert!(app.state.spawn_dock.is_some());
        assert_eq!(
            app.state.input_owner(),
            crate::app::state::InputOwner::SpawnDock
        );

        app.handle_key(TerminalKey::new(KeyCode::Tab, KeyModifiers::empty()))
            .await;
        assert_eq!(
            app.state.spawn_dock.as_ref().map(|dock| dock.focus),
            Some(crate::app::spawn_dock::SpawnDockField::Host)
        );
        for _ in 0..5 {
            app.handle_key(TerminalKey::new(KeyCode::Tab, KeyModifiers::empty()))
                .await;
        }
        assert_eq!(
            app.state.spawn_dock.as_ref().map(|dock| dock.focus),
            Some(crate::app::spawn_dock::SpawnDockField::Prompt)
        );
        for ch in "ship it".chars() {
            app.handle_key(TerminalKey::new(KeyCode::Char(ch), KeyModifiers::empty()))
                .await;
        }
        assert_eq!(
            app.state
                .spawn_dock
                .as_ref()
                .map(|dock| dock.home.prompt.as_str()),
            Some("ship it")
        );

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;
        assert!(app.state.spawn_dock.is_none());
        assert_eq!(
            crate::client::presentation::saved_spawn_dock_draft_for_test("test-spawn-dock")
                .map(|draft| draft.prompt),
            Some("ship it".into())
        );
        app.state.sidebar_presentation.save_spawn_dock_draft(None);
    }

    #[test]
    fn spawn_dock_mouse_click_focuses_prompt() {
        let mut app = app_with_test_workspaces(&["local"]);
        app.state.open_spawn_dock();
        app.state.set_server_mode(Mode::Terminal);
        app.state.view.terminal_area = Rect::new(0, 0, 100, 30);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 22,
            modifiers: KeyModifiers::empty(),
        });

        assert_eq!(
            app.state.spawn_dock.as_ref().map(|dock| dock.focus),
            Some(crate::app::spawn_dock::SpawnDockField::Prompt)
        );
    }

    fn app_with_remote_agent() -> (App, crate::api::schema::AgentRef) {
        let mut app = app_with_test_workspaces(&["local"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Idle);
        let local_entry = crate::ui::sidebar_thread_entries(&app.state)
            .into_iter()
            .next()
            .expect("local agent fixture");
        let agent_ref = crate::api::schema::AgentRef::new("ub2", "w3K:p11")
            .expect("valid remote agent reference");
        app.state.remote_agent_panel_entries = vec![std::sync::Arc::new(
            crate::ui::RemoteAgentPanelEntry::new(agent_ref.clone(), local_entry),
        )];
        (app, agent_ref)
    }

    #[test]
    fn close_remote_pane_uses_the_standard_confirmation_overlay() {
        let (mut app, agent_ref) = app_with_remote_agent();
        app.state.confirm_close = true;
        app.state.sidebar_selected_remote_agent = Some(agent_ref.clone());

        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Prefix);

        assert_eq!(
            app.state.client_overlay,
            crate::app::state::ClientOverlay::ConfirmClose
        );
        assert_eq!(app.state.confirm_close_remote_agent_ref, Some(agent_ref));
        assert_eq!(app.state.effective_interaction_mode(), Mode::ConfirmClose);
    }

    #[test]
    fn close_fleet_attach_pane_uses_owner_confirmation() {
        let (mut app, agent_ref) = app_with_remote_agent();
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        app.fleet_attach_agents.insert(pane_id, agent_ref.clone());
        app.state.confirm_close = true;

        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Prefix);

        assert_eq!(
            app.state.client_overlay,
            crate::app::state::ClientOverlay::ConfirmClose
        );
        assert_eq!(app.state.confirm_close_remote_agent_ref, Some(agent_ref));
        assert!(app.state.workspaces[0].pane_state(pane_id).is_some());
    }

    #[test]
    fn close_fleet_attach_pane_without_confirmation_keeps_local_pane() {
        let (mut app, agent_ref) = app_with_remote_agent();
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        app.fleet_attach_agents.insert(pane_id, agent_ref);
        app.state.confirm_close = false;

        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Prefix);

        assert!(app.state.workspaces[0].pane_state(pane_id).is_some());
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("ub2 pane action failed")
        );
        assert!(app
            .state
            .toast
            .as_ref()
            .unwrap()
            .context
            .contains("unreachable"));
    }

    #[test]
    fn confirming_fleet_attach_close_dispatches_owner_close() {
        let (mut app, agent_ref) = app_with_remote_agent();
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        app.fleet_attach_agents.insert(pane_id, agent_ref.clone());
        app.state.confirm_close = true;
        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Prefix);

        app.handle_confirm_close_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(app.state.workspaces[0].pane_state(pane_id).is_some());
        assert_eq!(
            app.state.client_overlay,
            crate::app::state::ClientOverlay::None
        );
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("ub2 pane action failed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_plain_local_final_pane_closes_workspace_when_remote_selection_was_cleared() {
        let (mut app, _) = app_with_remote_agent();
        app.state.confirm_close = false;
        app.state.sidebar_selected_remote_agent = None;

        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Prefix);

        assert!(app.state.workspaces.is_empty());
        assert_eq!(app.state.remote_agent_panel_entries.len(), 1);
    }

    #[test]
    fn focusing_a_local_pane_clears_remote_close_selection() {
        let (mut app, agent_ref) = app_with_remote_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.sidebar_selected_remote_agent = Some(agent_ref);

        app.focus_pane_internal_via_api(0, pane_id);

        assert!(app.state.sidebar_selected_remote_agent.is_none());
    }

    #[test]
    fn fleet_workspace_ac6_space_navigation_obeys_fleet_section_collapse() {
        let mut app = app_with_test_workspaces(&["local-one", "local-two"]);
        app.state.sidebar_group_mode = crate::app::state::SidebarGroupMode::Spaces;
        app.state.sidebar_sections_layout = true;
        app.state.sidebar_areas.hosts = true;
        for ws_idx in 0..2 {
            let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("local agent terminal")
                .set_detected_state(
                    Some(crate::detect::Agent::Claude),
                    crate::detect::AgentState::Blocked,
                );
        }
        let mut fleet = Workspace::test_new("fleet");
        fleet.is_fleet = true;
        app.state.workspaces.push(fleet);
        let fleet_idx = 2;
        let fleet_collapse_key = if app.state.sidebar_sections_layout {
            "sections:Fleet"
        } else {
            "repo:Fleet"
        };
        if app.state.sidebar_sections_layout {
            app.state
                .collapsed_sidebar_groups
                .insert(fleet_collapse_key.into());
        } else {
            app.state
                .collapsed_sidebar_groups
                .remove(fleet_collapse_key);
        }
        assert_eq!(app.state.visible_workspace_order(), vec![0, 1]);
        assert_eq!(
            app.state.workspace_navigation_order(),
            vec![0, 1, fleet_idx]
        );

        app.state.active = Some(1);
        assert_eq!(app.relative_visible_workspace(1), Some(fleet_idx));
        app.state.active = Some(0);
        assert_eq!(app.relative_visible_workspace(-1), Some(fleet_idx));

        if app.state.sidebar_sections_layout {
            app.state
                .collapsed_sidebar_groups
                .remove(fleet_collapse_key);
        } else {
            app.state
                .collapsed_sidebar_groups
                .insert(fleet_collapse_key.into());
        }
        app.state.active = Some(1);
        assert_eq!(app.relative_visible_workspace(1), Some(0));
        app.state.active = Some(0);
        assert_eq!(app.relative_visible_workspace(-1), Some(1));
    }

    #[cfg(unix)]
    fn assert_scratchpad_opens_as_real_pane(initially_zoomed: bool) {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let root = unique_temp_path("scratchpad-real-pane");
        fs::create_dir_all(&root).expect("create repository directory");
        let editor = root.join("editor.sh");
        fs::write(
            &editor,
            "printf '%s' \"$HERDR_PANE_ID\" > \"$1.identity\"\nread -r line\nprintf '%s' \"$line\" > \"$1.input\"\nread -r line\n",
        )
        .expect("write editor fixture");
        env.set("EDITOR", format!("/bin/sh '{}'", editor.display()));
        let mut app = app_with_test_workspaces(&["scratchpad"]);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.dock_home_focused = true;
        let workspace = &mut app.state.workspaces[0];
        workspace.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "scratchpad-test".into(),
            label: "scratchpad".into(),
            repo_root: root.clone(),
            checkout_path: root.clone(),
            is_linked_worktree: false,
        });
        let previous_focus = workspace.focused_pane_id().expect("focused pane");
        workspace.active_tab_mut().expect("active tab").zoomed = initially_zoomed;
        let path = crate::scratchpad::scratchpad_path(&root);
        crate::scratchpad::ensure_scratchpad_file(&path).expect("create scratchpad");
        fs::write(&path, "keep my notes\n").expect("seed scratchpad");

        app.execute_tui_navigate_action(NavigateAction::EditScratchpad, ActionContext::Prefix);

        let workspace = &app.state.workspaces[0];
        let editor_pane = workspace.focused_pane_id().expect("editor focus");
        assert_ne!(editor_pane, previous_focus);
        assert_eq!(app.find_pane(editor_pane).map(|(index, _)| index), Some(0));
        let tab = workspace.active_tab().expect("active tab");
        assert!(tab.layout.pane_ids().contains(&previous_focus));
        assert!(tab.layout.pane_ids().contains(&editor_pane));
        assert!(!tab.zoomed, "both panes must stay visible");
        let terminal_id = workspace
            .terminal_id(editor_pane)
            .expect("editor terminal")
            .clone();
        assert!(app.state.terminals.contains_key(&terminal_id));
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());
        let public_id = app
            .public_pane_id(0, editor_pane)
            .expect("public pane number");
        assert_eq!(
            wait_for_file(&path.with_extension("md.identity")),
            public_id
        );
        assert!(app.overlay_panes[&editor_pane].temp_files.is_empty());
        assert_eq!(
            fs::read_to_string(&path).expect("scratchpad"),
            "keep my notes\n"
        );
        assert!(
            !app.state.dock_home_focused,
            "editor pane owns keyboard focus"
        );
        for code in [KeyCode::Char('j'), KeyCode::Enter] {
            let key = TerminalKey::new(code, KeyModifiers::empty());
            assert!(!app.handle_dock_home_key_headless(&key));
            let target = app
                .handle_terminal_key_headless(key)
                .expect("key reaches pane");
            assert_eq!(target.terminal_id, terminal_id);
        }
        assert_eq!(wait_for_file(&path.with_extension("md.input")), "j");
        app.terminal_runtimes
            .remove(&terminal_id)
            .expect("runtime")
            .shutdown();
        fs::remove_dir_all(root).expect("clean fixture");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn edit_scratchpad_registers_a_managed_layout_pane_beside_the_focused_pane() {
        assert_scratchpad_opens_as_real_pane(false);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn edit_scratchpad_unzooms_to_keep_the_previously_focused_pane_visible() {
        assert_scratchpad_opens_as_real_pane(true);
    }

    #[test]
    fn edit_scratchpad_without_a_repository_preserves_dock_home_focus() {
        let mut app = app_with_test_workspaces(&["outside-repository"]);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.dock_home_focused = true;

        app.execute_tui_navigate_action(NavigateAction::EditScratchpad, ActionContext::Prefix);

        assert!(app.state.dock_home_focused);
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Home));
        assert!(app.overlay_panes.is_empty());
    }

    fn app_with_global_window_fixture() -> App {
        let mut app = app_with_test_workspaces(&["first", "second"]);
        app.state.workspaces[0].test_add_tab(Some("first-agentless"));
        app.state.workspaces[1].test_add_tab(Some("second-agentless"));
        app.state.ensure_test_terminals();
        app
    }

    fn active_window(state: &AppState) -> (usize, usize) {
        let workspace = state.active.expect("active workspace");
        (workspace, state.workspaces[workspace].active_tab_index())
    }

    fn assert_tui_window_cycle(app: &mut App, action: NavigateAction, expected: &[(usize, usize)]) {
        for expected_window in expected {
            app.execute_tui_navigate_action(action, ActionContext::Prefix);
            assert_eq!(active_window(&app.state), *expected_window);
        }
    }

    fn assert_headless_window_cycle(
        state: &mut AppState,
        action: NavigateAction,
        expected: &[(usize, usize)],
    ) {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        for expected_window in expected {
            execute_navigate_action_in_context(
                state,
                &mut terminal_runtimes,
                action,
                ActionContext::Prefix,
            );
            assert_eq!(active_window(state), *expected_window);
        }
    }

    #[test]
    fn global_window_cycle_includes_every_tab() {
        let mut app = app_with_global_window_fixture();
        let order = window_cycle_order(&app.state);
        let current = active_window(&app.state);
        let current_index = order.iter().position(|window| *window == current).unwrap();
        let forward = (1..=order.len())
            .map(|step| order[(current_index + step) % order.len()])
            .collect::<Vec<_>>();
        let backward = (1..=order.len())
            .map(|step| order[(current_index + order.len() - step % order.len()) % order.len()])
            .collect::<Vec<_>>();
        assert_tui_window_cycle(&mut app, NavigateAction::NextWindow, &forward);
        assert_tui_window_cycle(&mut app, NavigateAction::PreviousWindow, &backward);

        let mut state = app_with_global_window_fixture().state;
        let order = window_cycle_order(&state);
        let current = active_window(&state);
        let current_index = order.iter().position(|window| *window == current).unwrap();
        let forward = (1..=order.len())
            .map(|step| order[(current_index + step) % order.len()])
            .collect::<Vec<_>>();
        let backward = (1..=order.len())
            .map(|step| order[(current_index + order.len() - step % order.len()) % order.len()])
            .collect::<Vec<_>>();
        assert_headless_window_cycle(&mut state, NavigateAction::NextWindow, &forward);
        assert_headless_window_cycle(&mut state, NavigateAction::PreviousWindow, &backward);
    }

    #[test]
    fn window_cycle_follows_the_selected_view_order() {
        // ws0 and ws2 share a repo, ws1 does not. The Repo view walks the
        // worktree group together (0, 2, 1); the Spaces view has no grouping
        // and walks the Spaces as they are (0, 1, 2).
        let mut app = app_with_test_workspaces(&["main", "other", "worktree"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 2, "repo-key");
        app.state.ensure_test_terminals();

        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Repo);
        assert_eq!(window_cycle_order(&app.state), vec![(0, 0), (2, 0), (1, 0)],);

        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Spaces);
        assert_eq!(window_cycle_order(&app.state), vec![(0, 0), (1, 0), (2, 0)],);
    }

    #[test]
    fn window_cycle_order_still_reaches_every_tab() {
        let app = app_with_global_window_fixture();
        let mut order = window_cycle_order(&app.state);
        order.sort_unstable();
        assert_eq!(order, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn focus_sidebar_shortcuts_follow_filtered_and_expanded_rows() {
        let mut app = app_with_test_workspaces(&["blocked", "done", "working"]);
        app.state.sidebar_sections_layout = true;
        app.state.sidebar_group_mode = crate::app::state::SidebarGroupMode::Spaces;

        for (ws_idx, status) in [
            crate::detect::AgentState::Blocked,
            crate::detect::AgentState::Idle,
            crate::detect::AgentState::Working,
        ]
        .into_iter()
        .enumerate()
        {
            let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("agent terminal")
                .set_detected_state(Some(crate::detect::Agent::Claude), status);
            if ws_idx == 1 {
                app.state.workspaces[ws_idx].tabs[0]
                    .panes
                    .get_mut(&pane_id)
                    .expect("done pane")
                    .seen = false;
            }
        }

        let working = "Working";
        assert!(crate::ui::sidebar::section_is_collapsed(
            &app.state, working
        ));
        assert_eq!(app.state.visible_workspace_order(), vec![0, 1, 2]);
        assert_eq!(app.state.workspace_at_visible_position(2), Some(2));
        let third_workspace = navigate_reserved_action_for_key(
            &app.state,
            &TerminalKey::new(KeyCode::Char('3'), KeyModifiers::empty()),
        )
        .expect("workspace jump shortcut");
        assert_eq!(third_workspace, NavigateAction::SwitchWorkspace(2));
        execute_navigate_action(&mut app.state, third_workspace);
        assert_eq!(
            app.state.active,
            Some(2),
            "device rows have jump numbers even while Working is collapsed"
        );
        app.state.selected = 2;
        app.state.move_selected_workspace_by_visible_delta(-1);
        assert_eq!(app.state.selected, 1, "up follows the previous device row");
        app.state.selected = 2;
        app.state.move_selected_workspace_by_visible_delta(1);
        assert_eq!(app.state.selected, 2, "down stays at the final device row");

        app.state.toggle_sidebar_group(working);
        assert_eq!(app.state.visible_workspace_order(), vec![0, 1, 2]);
        let third_workspace = navigate_reserved_action_for_key(
            &app.state,
            &TerminalKey::new(KeyCode::Char('3'), KeyModifiers::empty()),
        )
        .expect("workspace jump shortcut");
        execute_navigate_action(&mut app.state, third_workspace);
        assert_eq!(
            app.state.active,
            Some(2),
            "expanded Working summary does not duplicate jump numbers"
        );

        app.state.sidebar_work_filter.query = "done".into();
        assert_eq!(app.state.visible_workspace_order(), vec![1]);
        assert_eq!(app.state.workspace_at_visible_position(0), Some(1));
        assert_eq!(app.state.workspace_at_visible_position(1), None);
        let second_workspace = navigate_reserved_action_for_key(
            &app.state,
            &TerminalKey::new(KeyCode::Char('2'), KeyModifiers::empty()),
        )
        .expect("workspace jump shortcut");
        execute_navigate_action(&mut app.state, second_workspace);
        assert_eq!(
            app.state.active,
            Some(2),
            "filtered workspaces have no jump number"
        );
        let first_workspace = navigate_reserved_action_for_key(
            &app.state,
            &TerminalKey::new(KeyCode::Char('1'), KeyModifiers::empty()),
        )
        .expect("workspace jump shortcut");
        execute_navigate_action(&mut app.state, first_workspace);
        assert_eq!(
            app.state.active,
            Some(1),
            "jump numbers follow filtered row order"
        );
    }

    #[test]
    fn fleet_workspace_ac6_window_cycle_scope_adds_fleet_only_when_selected() {
        let mut state = AppState::test_new();
        let mut local = Workspace::test_new("local");
        local.test_add_tab(Some("local-second"));
        let mut fleet = Workspace::test_new("fleet");
        fleet.is_fleet = true;
        state.workspaces = vec![local, Workspace::test_new("other-local"), fleet];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        let remote = remote_blocker("ub2", "fleet-agent");
        let agent_ref = remote.agent_ref.clone();
        state.remote_agent_panel_entries = vec![remote];

        let local_order = window_navigation_order(&state);
        assert!(!local_order.contains(&WindowCycleTarget::Remote(agent_ref.clone())));
        assert!(!local_order
            .iter()
            .any(|target| matches!(target, WindowCycleTarget::Local { ws_idx: 2, .. })));

        state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        let fleet_order = window_navigation_order(&state);
        assert!(fleet_order.contains(&WindowCycleTarget::Remote(agent_ref)));
        assert!(!fleet_order
            .iter()
            .any(|target| matches!(target, WindowCycleTarget::Local { ws_idx: 2, .. })));
    }

    #[test]
    fn fleet_workspace_ac6_window_cycle_opens_remote_fleet_tab_in_fleet_mode() {
        let mut config = Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "ub2".into(),
            target: "remote-ub2".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        let mut fleet = Workspace::test_new("fleet");
        fleet.is_fleet = true;
        app.state.workspaces = vec![Workspace::test_new("local"), fleet];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let remote = remote_blocker("ub2", "cycle-agent");
        let agent_ref = remote.agent_ref.clone();
        app.state.remote_agent_panel_entries = vec![remote];
        app.state.fleet_snapshot.hosts = vec![crate::fleet::HostSnapshot {
            name: "ub2".into(),
            target: "remote-ub2".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }];
        let argv = crate::fleet::agent_attach_argv_from_config(
            &config.remote.fleet.hosts[0],
            "cycle-agent",
        )
        .expect("configured remote attach argv");
        let fleet_tab = &app.state.workspaces[1].tabs[0];
        let fleet_root_pane = fleet_tab.root_pane;
        let fleet_terminal_id = fleet_tab
            .terminal_id(fleet_tab.root_pane)
            .expect("fleet terminal id")
            .clone();
        app.state
            .terminals
            .get_mut(&fleet_terminal_id)
            .expect("fleet terminal")
            .launch_argv = Some(argv);

        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        app.focus_relative_window(true);

        assert_eq!(app.state.active, Some(1));
        assert_eq!(
            app.state.workspaces[1].focused_pane_id(),
            Some(fleet_root_pane)
        );
        assert_eq!(app.state.sidebar_selected_remote_agent, Some(agent_ref));
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_space_excludes_its_cycle_targets() {
        let mut app = app_with_test_workspaces(&["main", "other", "worktree"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 2, "repo-key");
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Repo);
        app.state.collapsed_space_keys.insert("repo-key".into());
        let blocked_pane = app.state.workspaces[2].tabs[0].root_pane;
        set_tab_agent_state(&mut app.state, 2, 0, crate::detect::AgentState::Blocked);

        app.state.skip_collapsed_cycle = false;
        assert!(window_cycle_order(&app.state).contains(&(2, 0)));
        assert!(blocked_pane_cycle_in_order(&app.state)
            .iter()
            .any(|(target, _)| matches!(
                target,
                BlockedPaneTarget::Local { ws_idx: 2, pane_id, .. } if *pane_id == blocked_pane
            )));
        app.state.skip_collapsed_cycle = true;
        // The group fold hides members from the space list; the sidebar may
        // still list their agents, and the cycles follow the sidebar.
        let rows = crate::ui::sidebar_rows(&app.state);
        let shown = rows.iter().any(|row| {
            matches!(
                row,
                crate::ui::SidebarRow::Tab { entry, .. } | crate::ui::SidebarRow::Agent { entry, .. }
                    if entry.local_target().is_some_and(|t| (t.ws_idx, t.tab_idx) == (2, 0))
            )
        });
        let order = window_cycle_order(&app.state);
        assert_eq!(order.contains(&(2, 0)), shown, "{order:?}");
        assert!(order.contains(&(0, 0)));
        let other_shown = rows.iter().any(|row| {
            matches!(
                row,
                crate::ui::SidebarRow::Tab { entry, .. } | crate::ui::SidebarRow::Agent { entry, .. }
                    if entry.local_target().is_some_and(|t| (t.ws_idx, t.tab_idx) == (1, 0))
            )
        });
        assert_eq!(
            order.contains(&(1, 0)),
            other_shown,
            "cycle membership follows visible rows: {order:?}"
        );
        assert_eq!(
            blocked_pane_cycle_in_order(&app.state)
                .iter()
                .any(|(target, _)| matches!(
                    target,
                    BlockedPaneTarget::Local { ws_idx: 2, pane_id, .. } if *pane_id == blocked_pane
                )),
            shown
        );
    }

    #[test]
    fn skip_collapsed_cycle_hides_remote_agents_in_collapsed_device_groups() {
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("local")];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.agent_host_name = "main".into();
        state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        state.skip_collapsed_cycle = true;
        state.sidebar_sections_layout = true;
        let remote = remote_blocker("ub2", "collapsed-agent");
        let agent_ref = remote.agent_ref.clone();
        state.remote_agent_panel_entries = vec![remote];
        state.remote_agent_device_groups =
            Some(crate::ui::sidebar::remote_agent_device_groups(&state));
        state
            .collapsed_sidebar_groups
            .insert("device:main/ub2".into());

        assert!(!crate::ui::sidebar_rows(&state)
            .iter()
            .any(|row| matches!(row, crate::ui::SidebarRow::RemoteAgent { entry, .. } if entry.agent_ref == agent_ref)));
        let window_targets = window_navigation_order(&state);
        assert!(!window_targets.contains(&WindowCycleTarget::Remote(agent_ref.clone())));
    }

    #[test]
    fn fleet_window_cycle_includes_remote_agents_in_collapsed_device_groups() {
        let mut state = AppState::test_new();
        state.agent_host_name = "ub2".into();
        state.workspaces = vec![Workspace::test_new("local")];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        state.skip_collapsed_cycle = false;
        state.sidebar_sections_layout = true;
        let remotes = ["ub1", "mbpro"]
            .into_iter()
            .map(|host| remote_blocker(host, "worker"))
            .collect::<Vec<_>>();
        let expected = remotes
            .iter()
            .map(|remote| WindowCycleTarget::Remote(remote.agent_ref.clone()))
            .collect::<Vec<_>>();
        state.remote_agent_panel_entries = remotes;
        state.remote_agent_device_groups =
            Some(crate::ui::sidebar::remote_agent_device_groups(&state));
        for host in ["ub1", "mbpro"] {
            state
                .collapsed_sidebar_groups
                .insert(format!("device:main/{host}"));
        }

        assert_eq!(
            window_navigation_order(&state)
                .into_iter()
                .filter(|target| matches!(target, WindowCycleTarget::Remote(_)))
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn next_window_opens_remote_agent_hidden_in_collapsed_device_group() {
        let (mut app, agent_ref) = app_with_remote_agent();
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        app.state.skip_collapsed_cycle = false;
        app.state.sidebar_sections_layout = true;
        app.state.remote_agent_device_groups =
            Some(crate::ui::sidebar::remote_agent_device_groups(&app.state));
        app.state
            .collapsed_sidebar_groups
            .insert(format!("device:main/{}", agent_ref.host));

        assert!(!crate::ui::sidebar_rows(&app.state)
            .iter()
            .any(|row| matches!(
                row,
                crate::ui::SidebarRow::RemoteAgent { entry, .. } if entry.agent_ref == agent_ref
            )));

        app.focus_relative_window(true);

        assert_eq!(app.state.sidebar_selected_remote_agent, Some(agent_ref));
        assert_eq!(app.remote_focus_operations.len(), 1);
        assert!(app.state.toast.is_none());
    }

    #[test]
    fn next_window_to_remote_clears_home_view() {
        let (mut app, _) = app_with_remote_agent();
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        app.state.home = Some(crate::app::home::HomeState::default());

        app.focus_relative_window(true);

        assert!(app.state.home.is_none());
        assert!(app.state.active.is_some());
    }

    #[test]
    fn next_blocked_window_to_remote_clears_home_view() {
        let mut app = app_with_global_window_fixture();
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        let remote = remote_blocker("ub2", "blocked-agent");
        let agent_ref = remote.agent_ref.clone();
        app.state.remote_agent_panel_entries = vec![remote];
        app.state.fleet_snapshot.hosts = vec![crate::fleet::HostSnapshot {
            name: "ub2".into(),
            target: "remote-ub2".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }];
        app.state.home = Some(crate::app::home::HomeState::default());

        app.focus_next_blocked_window();

        assert!(app.state.home.is_none());
        assert_eq!(app.state.sidebar_selected_remote_agent, Some(agent_ref));
    }

    #[test]
    fn next_agent_opens_remote_agent_hidden_in_collapsed_device_group() {
        let (mut app, agent_ref) = app_with_remote_agent();
        app.state.sidebar_sections_layout = true;
        app.state.remote_agent_device_groups =
            Some(crate::ui::sidebar::remote_agent_device_groups(&app.state));
        app.state
            .collapsed_sidebar_groups
            .insert(format!("device:main/{}", agent_ref.host));

        assert!(!crate::ui::sidebar_rows(&app.state)
            .iter()
            .any(|row| matches!(
                row,
                crate::ui::SidebarRow::RemoteAgent { entry, .. } if entry.agent_ref == agent_ref
            )));

        app.execute_tui_navigate_action(NavigateAction::NextAgent, ActionContext::Prefix);

        assert_eq!(app.state.sidebar_selected_remote_agent, Some(agent_ref));
        assert_eq!(app.remote_focus_operations.len(), 1);
        assert!(app.state.toast.is_none());
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_spaces_section_excludes_local_windows() {
        let mut app = app_with_global_window_fixture();
        for terminal in app.state.terminals.values_mut() {
            terminal.set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Blocked,
            );
        }
        app.state.skip_collapsed_cycle = true;
        assert!(!window_navigation_order(&app.state).is_empty());
        assert!(!blocked_pane_cycle_in_order(&app.state).is_empty());

        let key = format!(
            "{}:{}",
            app.state.sidebar_group_mode.collapse_namespace(),
            crate::ui::sidebar::SPACES_SECTION_TITLE
        );
        app.state.collapsed_sidebar_groups.insert(key);
        assert!(crate::ui::sidebar::section_is_collapsed(
            &app.state,
            crate::ui::sidebar::SPACES_SECTION_TITLE
        ));
        assert!(window_navigation_order(&app.state).is_empty());
        assert!(blocked_pane_cycle_in_order(&app.state).is_empty());
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_sections_exclude_their_tabs() {
        let mut app = app_with_global_window_fixture();
        for terminal in app.state.terminals.values_mut() {
            terminal.set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Blocked,
            );
        }
        app.state.sidebar_sections_layout = true;
        app.state.skip_collapsed_cycle = true;
        assert!(!window_navigation_order(&app.state).is_empty());

        // Collapse every section header the sidebar shows.
        for _ in 0..3 {
            let titles = crate::ui::sidebar_rows(&app.state)
                .into_iter()
                .filter_map(|row| match row {
                    crate::ui::SidebarRow::SectionHeader {
                        title,
                        collapsed: false,
                        ..
                    } => Some(title.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for title in titles {
                let key = format!("sections:{title}");
                if !app.state.collapsed_sidebar_groups.remove(&key) {
                    app.state.collapsed_sidebar_groups.insert(key);
                }
            }
        }
        // The sections layout shows tabs only through their own rows.
        let shown = crate::ui::sidebar_rows(&app.state)
            .into_iter()
            .filter_map(|row| match row {
                crate::ui::SidebarRow::Tab { entry, .. } => entry.local_target(),
                _ => None,
            })
            .map(|target| (target.ws_idx, target.tab_idx))
            .collect::<std::collections::HashSet<_>>();
        let cycled = window_cycle_order(&app.state)
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(cycled, shown);

        app.state.skip_collapsed_cycle = false;
        assert!(!window_navigation_order(&app.state).is_empty());
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_plain_layout_skips_hidden_agent_tabs() {
        let mut app = app_with_global_window_fixture();
        for terminal in app.state.terminals.values_mut() {
            terminal.set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        }
        app.state.skip_collapsed_cycle = true;
        let expanded = window_navigation_order(&app.state);
        assert!(!expanded.is_empty());

        let namespace = app.state.sidebar_group_mode.collapse_namespace();
        for title in [
            crate::ui::sidebar::SNOOZED_SECTION_TITLE,
            crate::ui::sidebar::SETTLED_SECTION_TITLE,
            "Active",
            "Pinned",
        ] {
            app.state
                .collapsed_sidebar_groups
                .insert(format!("{namespace}:{title}"));
        }
        let rows = crate::ui::sidebar_rows(&app.state);
        let order = window_navigation_order(&app.state);
        for target in &order {
            let WindowCycleTarget::Local { ws_idx, tab_idx } = target else {
                continue;
            };
            assert!(
                rows.iter().any(|row| matches!(
                    row,
                    crate::ui::SidebarRow::Tab { entry, .. }
                        if entry.local_target().is_some_and(|t| (t.ws_idx, t.tab_idx) == (*ws_idx, *tab_idx))
                )),
                "agent tab {ws_idx}:{tab_idx} is cycled without a visible row"
            );
        }
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_keeps_tabs_in_work_item_modes() {
        for mode in [
            crate::app::state::SidebarGroupMode::LinearTeam,
            crate::app::state::SidebarGroupMode::Missive,
        ] {
            let mut app = app_with_global_window_fixture();
            for terminal in app.state.terminals.values_mut() {
                terminal.set_detected_state(
                    Some(crate::detect::Agent::Claude),
                    crate::detect::AgentState::Blocked,
                );
            }
            app.state.set_sidebar_group_mode(mode);
            app.state.skip_collapsed_cycle = true;
            assert!(!window_navigation_order(&app.state).is_empty(), "{mode:?}");
            assert!(
                !blocked_pane_cycle_in_order(&app.state).is_empty(),
                "{mode:?}"
            );

            // One agentless tab, then collapse every work-item group.
            if let Some(terminal) = app.state.terminals.values_mut().next() {
                terminal.set_detected_state(None, crate::detect::AgentState::Idle);
            }
            let namespace = app.state.sidebar_group_mode.collapse_namespace();
            let keys = crate::ui::sidebar::workspace_list_entries_for_mode(&app.state, false, mode)
                .into_iter()
                .filter_map(|entry| match entry {
                    crate::ui::sidebar::WorkspaceListEntry::NestedHeader { key, .. } => Some(key),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(!keys.is_empty(), "{mode:?}");
            for key in keys {
                app.state
                    .collapsed_sidebar_groups
                    .insert(format!("{namespace}:{key}"));
            }
            let rows = crate::ui::sidebar_rows(&app.state);
            let order = window_navigation_order(&app.state);
            for target in &order {
                let WindowCycleTarget::Local { ws_idx, tab_idx } = target else {
                    continue;
                };
                assert!(
                    rows.iter().any(|row| matches!(
                        row,
                        crate::ui::SidebarRow::Tab { entry, .. }
                            if entry.local_target().is_some_and(|t| (t.ws_idx, t.tab_idx) == (*ws_idx, *tab_idx))
                    )),
                    "{mode:?}: tab {ws_idx}:{tab_idx} is cycled without a visible row"
                );
            }
        }
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_cycles_only_shown_tabs_in_every_mode() {
        use crate::app::state::SidebarGroupMode;
        for sections in [false, true] {
            for mode in [
                SidebarGroupMode::Repo,
                SidebarGroupMode::RepoWorktree,
                SidebarGroupMode::Spaces,
                SidebarGroupMode::RepoPr,
                SidebarGroupMode::LinearTeam,
                SidebarGroupMode::Missive,
            ] {
                let mut app = app_with_global_window_fixture();
                mark_worktree_space_member(&mut app.state, 0, "repo-key");
                mark_worktree_space_member(&mut app.state, 1, "repo-key");
                for terminal in app.state.terminals.values_mut() {
                    terminal.set_detected_state(
                        Some(crate::detect::Agent::Claude),
                        crate::detect::AgentState::Blocked,
                    );
                }
                // One agentless tab: it has no row of its own.
                if let Some(terminal) = app.state.terminals.values_mut().next() {
                    terminal.set_detected_state(None, crate::detect::AgentState::Idle);
                }
                app.state.sidebar_sections_layout = sections;
                app.state.set_sidebar_group_mode(mode);
                app.state.skip_collapsed_cycle = true;
                let expanded = window_navigation_order(&app.state);

                // Collapse every group header except the Spaces section.
                let namespace = mode.collapse_namespace();
                app.state.collapsed_space_keys.insert("repo-key".into());
                for _ in 0..3 {
                    let mut keys = crate::ui::sidebar_rows(&app.state)
                        .into_iter()
                        .filter_map(|row| match row {
                            crate::ui::SidebarRow::Workspace {
                                count: Some(_),
                                sort_key: Some(key),
                                ..
                            } => Some(key),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    keys.extend(
                        crate::ui::sidebar::workspace_list_entries_for_mode(
                            &app.state, false, mode,
                        )
                        .into_iter()
                        .filter_map(|entry| match entry {
                            crate::ui::sidebar::WorkspaceListEntry::NestedHeader {
                                key, ..
                            } => Some(key),
                            _ => None,
                        }),
                    );
                    for key in keys {
                        app.state
                            .collapsed_sidebar_groups
                            .insert(format!("{namespace}:{key}"));
                    }
                }
                let rows = crate::ui::sidebar_rows(&app.state);
                let order = window_navigation_order(&app.state);
                // Every tab the sidebar still shows keeps its place.
                for row in &rows {
                    let (crate::ui::SidebarRow::Tab { entry, .. }
                    | crate::ui::SidebarRow::Agent { entry, .. }) = row
                    else {
                        continue;
                    };
                    let Some(t) = entry.local_target() else {
                        continue;
                    };
                    assert!(
                        order.contains(&WindowCycleTarget::Local {
                            ws_idx: t.ws_idx,
                            tab_idx: t.tab_idx
                        }),
                        "{mode:?} sections={sections}: shown tab {t:?} skipped"
                    );
                }
                for target in &order {
                    assert!(expanded.contains(target), "{mode:?} sections={sections}");
                    let WindowCycleTarget::Local { ws_idx, tab_idx } = *target else {
                        continue;
                    };
                    let shown = rows.iter().any(|row| match row {
                        crate::ui::SidebarRow::Tab { entry, .. } => entry
                            .local_target()
                            .is_some_and(|t| (t.ws_idx, t.tab_idx) == (ws_idx, tab_idx)),
                        crate::ui::SidebarRow::Workspace {
                            ws_idx: row_ws,
                            count: None,
                            ..
                        } => {
                            !sections
                                && (*row_ws == ws_idx
                                    || (mode != SidebarGroupMode::Spaces
                                        && crate::ui::sidebar::sidebar_space_member_indices(
                                            &app.state, *row_ws,
                                        )
                                        .contains(&ws_idx)))
                        }
                        _ => false,
                    });
                    assert!(
                        shown,
                        "{mode:?} sections={sections}: tab {ws_idx}:{tab_idx} is cycled without a shown row"
                    );
                }
            }
        }
    }

    #[test]
    fn window_cycle_order_matches_sidebar_sort_and_fleet_row_order() {
        use crate::app::state::SidebarSortMode;
        let mut app = app_with_global_window_fixture();
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        app.state.skip_collapsed_cycle = true;
        for terminal in app.state.terminals.values_mut() {
            terminal.set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        }
        set_window_agent_state(&mut app.state, (0, 1), crate::detect::AgentState::Blocked);
        let remote = remote_blocker("ub2", "sidebar-order");
        app.state.remote_agent_panel_entries = vec![remote];
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Spaces);

        let mut observed = Vec::new();
        for sort in [SidebarSortMode::Status, SidebarSortMode::Recent] {
            for workspace in &app.state.workspaces {
                app.state
                    .sidebar_group_sorts
                    .insert(format!("space:{}", workspace.id), sort);
            }
            let rows = crate::ui::sidebar_rows(&app.state);
            let expected = rows
                .iter()
                .filter_map(|row| match row {
                    crate::ui::SidebarRow::Tab { entry, .. }
                    | crate::ui::SidebarRow::Agent { entry, .. } => {
                        entry.local_target().map(|target| WindowCycleTarget::Local {
                            ws_idx: target.ws_idx,
                            tab_idx: target.tab_idx,
                        })
                    }
                    crate::ui::SidebarRow::RemoteAgent { entry, .. } => {
                        Some(WindowCycleTarget::Remote(entry.agent_ref.clone()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let actual = window_navigation_order(&app.state);
            assert_eq!(actual, expected, "{sort:?}");
            observed.push(actual);
        }
        assert_eq!(
            observed.len(),
            2,
            "Status and Recent sort modes were covered"
        );
    }

    #[test]
    fn needs_you_duplicates_do_not_change_window_cycle_order() {
        let mut app = app_with_global_window_fixture();
        app.state.skip_collapsed_cycle = true;
        for terminal in app.state.terminals.values_mut() {
            terminal.set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Blocked,
            );
        }
        let rows = crate::ui::sidebar_rows(&app.state);
        assert!(rows
            .iter()
            .any(|row| matches!(row, crate::ui::SidebarRow::NeedsYou { .. })));
        let expected = rows
            .iter()
            .filter_map(|row| match row {
                crate::ui::SidebarRow::Tab { entry, .. }
                | crate::ui::SidebarRow::Agent { entry, .. } => {
                    entry.local_target().map(|target| WindowCycleTarget::Local {
                        ws_idx: target.ws_idx,
                        tab_idx: target.tab_idx,
                    })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(window_navigation_order(&app.state), expected);
    }

    #[test]
    fn frozen_window_cycle_order_survives_sort_changes_and_refreshes_after_idle_or_input() {
        use crate::app::state::SidebarSortMode;
        let mut app = app_with_global_window_fixture();
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Spaces);
        set_window_agent_state(&mut app.state, (0, 1), crate::detect::AgentState::Blocked);
        for workspace in &app.state.workspaces {
            app.state
                .sidebar_group_sorts
                .insert(format!("space:{}", workspace.id), SidebarSortMode::Status);
        }
        let now = Instant::now();
        let frozen = app.window_cycle_order_at(now);
        for workspace in &app.state.workspaces {
            app.state
                .sidebar_group_sorts
                .insert(format!("space:{}", workspace.id), SidebarSortMode::Recent);
        }
        assert_ne!(window_navigation_order(&app.state), frozen);

        assert_eq!(
            app.window_cycle_order_at(now + Duration::from_secs(1)),
            frozen
        );
        assert_eq!(
            app.window_cycle_order_at(now + Duration::from_secs(2)),
            frozen
        );
        assert_eq!(
            app.window_cycle_order_at(now + Duration::from_secs(3)),
            frozen
        );

        let refreshed =
            app.window_cycle_order_at(now + Duration::from_secs(3) + WINDOW_CYCLE_SNAPSHOT_IDLE);
        assert_eq!(refreshed, window_navigation_order(&app.state));
        app.execute_tui_navigate_action(NavigateAction::ToggleSidebar, ActionContext::Prefix);
        app.state.sidebar_group_sorts.clear();
        assert_eq!(
            app.window_cycle_order_at(
                now + Duration::from_secs(3)
                    + WINDOW_CYCLE_SNAPSHOT_IDLE
                    + Duration::from_millis(1)
            ),
            window_navigation_order(&app.state)
        );
    }

    #[tokio::test]
    async fn window_cycle_snapshot_survives_prefix_chords_and_refreshes_after_other_action() {
        use crate::app::state::SidebarSortMode;

        let mut app = app_with_global_window_fixture();
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Spaces);
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachine;
        app.state.sidebar_collapsed = true;
        app.state.sidebar_focused = false;
        app.state.focus_client_on_pane();
        app.state.prefix_code = KeyCode::Char('a');
        app.state.prefix_mods = KeyModifiers::CONTROL;
        app.state.keybinds.next_window = crate::config::ActionKeybinds::prefix("n");
        for workspace in &app.state.workspaces {
            app.state
                .sidebar_group_sorts
                .insert(format!("space:{}", workspace.id), SidebarSortMode::Status);
        }
        set_window_agent_state(&mut app.state, (0, 1), crate::detect::AgentState::Blocked);
        let initial_order = window_navigation_order(&app.state);
        assert!(initial_order.len() >= 3);

        let prefix = TerminalKey::new(app.state.prefix_code, app.state.prefix_mods);
        let next = TerminalKey::new(KeyCode::Char('n'), KeyModifiers::empty());
        let passthrough = TerminalKey::new(KeyCode::F(12), KeyModifiers::empty());
        let initial_active = active_window(&app.state);
        let initial_index = initial_order
            .iter()
            .position(|target| {
                *target
                    == WindowCycleTarget::Local {
                        ws_idx: initial_active.0,
                        tab_idx: initial_active.1,
                    }
            })
            .expect("active window in initial order");
        let frozen_targets = (1..=3)
            .map(
                |step| match initial_order[(initial_index + step) % initial_order.len()] {
                    WindowCycleTarget::Local { ws_idx, tab_idx } => (ws_idx, tab_idx),
                    WindowCycleTarget::Remote(_) => panic!("fixture has only local windows"),
                },
            )
            .collect::<Vec<_>>();
        async fn press(app: &mut App, key: TerminalKey) {
            app.handle_key(key).await;
        }
        press(&mut app, prefix.clone()).await;
        press(&mut app, next.clone()).await;
        let target_a = active_window(&app.state);
        assert_eq!(target_a, frozen_targets[0]);

        for workspace in &app.state.workspaces {
            app.state
                .sidebar_group_sorts
                .insert(format!("space:{}", workspace.id), SidebarSortMode::Recent);
        }
        let changed_order = window_navigation_order(&app.state);
        assert_ne!(changed_order, initial_order);

        for expected in frozen_targets.iter().copied().skip(1) {
            press(&mut app, prefix.clone()).await;
            press(&mut app, next.clone()).await;
            assert_eq!(active_window(&app.state), expected);
        }

        press(&mut app, prefix.clone()).await;
        press(&mut app, passthrough).await;
        let before_refresh_cycle = active_window(&app.state);
        press(&mut app, prefix).await;
        press(&mut app, next).await;
        let refreshed_index = changed_order
            .iter()
            .position(|target| {
                *target
                    == WindowCycleTarget::Local {
                        ws_idx: before_refresh_cycle.0,
                        tab_idx: before_refresh_cycle.1,
                    }
            })
            .expect("active window in refreshed order");
        assert_eq!(
            active_window(&app.state),
            match changed_order[(refreshed_index + 1) % changed_order.len()] {
                WindowCycleTarget::Local { ws_idx, tab_idx } => (ws_idx, tab_idx),
                WindowCycleTarget::Remote(_) => panic!("fixture has only local windows"),
            }
        );
        assert_eq!(window_navigation_order(&app.state), changed_order);
    }

    #[test]
    fn frozen_window_cycle_drops_tabs_removed_during_the_burst() {
        let mut app = app_with_global_window_fixture();
        let now = Instant::now();
        assert!(app
            .window_cycle_order_at(now)
            .contains(&WindowCycleTarget::Local {
                ws_idx: 0,
                tab_idx: 1,
            }));
        app.state.workspaces[0].tabs.pop();
        assert!(!app
            .window_cycle_order_at(now + Duration::from_millis(20))
            .contains(&WindowCycleTarget::Local {
                ws_idx: 0,
                tab_idx: 1,
            }));
    }

    #[test]
    fn frozen_window_cycle_resolves_survivors_after_an_earlier_tab_closes() {
        let mut app = app_with_global_window_fixture();
        let now = Instant::now();
        let frozen = app.window_cycle_order_at(now);
        let roots = |app: &App, order: &[WindowCycleTarget]| {
            order
                .iter()
                .filter_map(|target| match target {
                    WindowCycleTarget::Local { ws_idx, tab_idx } => app
                        .state
                        .workspaces
                        .get(*ws_idx)
                        .and_then(|workspace| workspace.tabs.get(*tab_idx))
                        .map(|tab| tab.root_pane),
                    WindowCycleTarget::Remote(_) => None,
                })
                .collect::<Vec<_>>()
        };
        let expected = roots(&app, &frozen);
        let removed = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs.remove(0);

        let surviving = app.window_cycle_order_at(now + Duration::from_millis(20));
        let expected: Vec<_> = expected
            .into_iter()
            .filter(|root| *root != removed)
            .collect();
        assert_eq!(roots(&app, &surviving), expected);
    }

    fn set_window_agent_state(
        state: &mut AppState,
        (ws_idx, tab_idx): (usize, usize),
        status: crate::detect::AgentState,
    ) {
        let tab = &state.workspaces[ws_idx].tabs[tab_idx];
        let terminal_id = tab
            .terminal_id(tab.root_pane)
            .expect("tab terminal")
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .set_detected_state(Some(crate::detect::Agent::Claude), status);
        for (current_ws_idx, workspace) in state.workspaces.iter().enumerate() {
            for (current_tab_idx, tab) in workspace.tabs.iter().enumerate() {
                let terminal_id = tab.terminal_id(tab.root_pane).expect("tab terminal");
                state
                    .terminals
                    .get_mut(terminal_id)
                    .expect("terminal state")
                    .last_agent_state_change_seq =
                    Some(if (current_ws_idx, current_tab_idx) == (ws_idx, tab_idx) {
                        1
                    } else {
                        100
                    });
            }
        }
    }

    #[test]
    fn next_window_matches_sidebar_order_for_worktree_modes() {
        use crate::app::state::SidebarGroupMode;
        for sections in [false, true] {
            for mode in [
                SidebarGroupMode::Repo,
                SidebarGroupMode::RepoWorktree,
                SidebarGroupMode::RepoPr,
            ] {
                // Root `a`, unrelated `b`, then `a`'s worktree.
                let mut app = app_with_test_workspaces(&["a", "b", "a-wt"]);
                mark_worktree_space_member(&mut app.state, 0, "repo-a");
                mark_worktree_space_member(&mut app.state, 2, "repo-a");
                for terminal in app.state.terminals.values_mut() {
                    terminal.set_detected_state(
                        Some(crate::detect::Agent::Claude),
                        crate::detect::AgentState::Blocked,
                    );
                }
                app.state.sidebar_sections_layout = sections;
                app.state.set_sidebar_group_mode(mode);

                // Nothing is collapsed, so skip-collapsed keeps every shown tab.
                app.state.skip_collapsed_cycle = true;
                let cycled = window_cycle_order(&app.state);
                let shown_rows = crate::ui::sidebar_rows(&app.state)
                    .into_iter()
                    .filter_map(|row| match row {
                        crate::ui::SidebarRow::Tab { entry, .. }
                        | crate::ui::SidebarRow::Agent { entry, .. } => entry.local_target(),
                        _ => None,
                    })
                    .map(|target| (target.ws_idx, target.tab_idx))
                    .collect::<Vec<_>>();
                assert_eq!(
                    cycled, shown_rows,
                    "{mode:?} sections={sections}: cycle order must match the sidebar"
                );
                let blocked = blocked_pane_cycle_in_order(&app.state);
                for row in crate::ui::sidebar_rows(&app.state) {
                    let (crate::ui::SidebarRow::Tab { entry, .. }
                    | crate::ui::SidebarRow::Agent { entry, .. }) = row
                    else {
                        continue;
                    };
                    let Some(target) = entry.local_target() else {
                        continue;
                    };
                    assert!(
                        blocked.iter().any(|(pane, _)| matches!(
                            pane,
                            BlockedPaneTarget::Local { ws_idx, pane_id, .. }
                                if (*ws_idx, *pane_id) == (target.ws_idx, target.pane_id)
                        )),
                        "{mode:?} sections={sections}: shown blocked pane {target:?} skipped"
                    );
                }
            }
        }
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_keeps_snoozed_tabs_shown_elsewhere() {
        let spaces = crate::ui::sidebar::SPACES_SECTION_TITLE;
        let snoozed = crate::ui::sidebar::SNOOZED_SECTION_TITLE;
        for sections in [false, true] {
            let mut app = app_with_global_window_fixture();
            for terminal in app.state.terminals.values_mut() {
                terminal.set_detected_state(
                    Some(crate::detect::Agent::Claude),
                    crate::detect::AgentState::Idle,
                );
            }
            app.state.sidebar_sections_layout = sections;
            let pane_id = crate::ui::sidebar_thread_entries(&app.state)
                .into_iter()
                .filter_map(|entry| entry.local_target())
                .find(|target| (target.ws_idx, target.tab_idx) == (0, 0))
                .expect("agent row for tab 0:0")
                .pane_id;
            assert!(app.state.snooze_pane_at(0, pane_id, u64::MAX / 2));
            let namespace = if sections {
                "sections".to_string()
            } else {
                app.state
                    .sidebar_group_mode
                    .collapse_namespace()
                    .to_string()
            };
            for (title, collapsed) in [(spaces, true), (snoozed, false)] {
                let key = format!("{namespace}:{title}");
                if crate::ui::sidebar::section_is_collapsed(&app.state, title) != collapsed
                    && !app.state.collapsed_sidebar_groups.remove(&key)
                {
                    app.state.collapsed_sidebar_groups.insert(key);
                }
            }
            app.state.skip_collapsed_cycle = true;
            let shown = crate::ui::sidebar_rows(&app.state).into_iter().any(|row| {
                matches!(
                    row,
                    crate::ui::SidebarRow::Tab { entry, .. } | crate::ui::SidebarRow::Agent { entry, .. }
                        if entry.local_target().is_some_and(|t| (t.ws_idx, t.tab_idx) == (0, 0))
                )
            });
            assert_eq!(
                window_cycle_order(&app.state).contains(&(0, 0)),
                shown,
                "sections={sections}: snoozed tab cycle membership must follow its row"
            );
            assert!(
                shown,
                "sections={sections}: fixture should show the snoozed tab"
            );
        }
    }

    #[test]
    fn fleet_workspace_ac9_skip_collapsed_keeps_both_blockers_in_a_split_tab() {
        for sections in [false, true] {
            let mut app = app_with_test_workspaces(&["split"]);
            app.state.active = Some(0);
            let first = app.state.workspaces[0].tabs[0].root_pane;
            let second = app.state.workspaces[0].test_split(Direction::Horizontal);
            app.state.ensure_test_terminals();
            for pane in [first, second] {
                set_pane_agent_state(
                    &mut app.state,
                    0,
                    0,
                    pane,
                    crate::detect::AgentState::Blocked,
                );
            }
            app.state.sidebar_sections_layout = sections;
            app.state.skip_collapsed_cycle = true;
            let blocked = blocked_pane_cycle_in_order(&app.state)
                .into_iter()
                .filter_map(|(target, _)| match target {
                    BlockedPaneTarget::Local { pane_id, .. } => Some(pane_id),
                    BlockedPaneTarget::Remote(_) => None,
                })
                .collect::<Vec<_>>();
            assert!(
                blocked.contains(&first) && blocked.contains(&second),
                "sections={sections}: {blocked:?}"
            );
        }
    }

    #[test]
    fn global_window_cycle_ignores_presentation_state() {
        let mut app = app_with_global_window_fixture();
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.sidebar_collapsed = true;
        app.state.collapsed_space_keys.insert("repo-key".into());
        app.state.collapsed_sidebar_groups.insert("agents".into());
        app.state.workspaces[0].identity_cwd = "/changed/first".into();
        app.state.workspaces[1].identity_cwd = "/changed/second".into();
        for terminal in app.state.terminals.values_mut() {
            terminal.cwd = "/changed/terminal".into();
            terminal.set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Blocked,
            );
        }

        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextWindow,
            &[(0, 1), (1, 0), (1, 1), (0, 0)],
        );

        let mut state = app.state;
        assert_headless_window_cycle(
            &mut state,
            NavigateAction::PreviousWindow,
            &[(1, 1), (1, 0), (0, 1), (0, 0)],
        );
    }

    fn set_tab_agent_state(
        state: &mut AppState,
        workspace: usize,
        tab: usize,
        agent_state: crate::detect::AgentState,
    ) {
        let root_pane = state.workspaces[workspace].tabs[tab].root_pane;
        set_pane_agent_state(state, workspace, tab, root_pane, agent_state);
    }

    fn set_pane_agent_state(
        state: &mut AppState,
        workspace: usize,
        tab: usize,
        pane: crate::layout::PaneId,
        agent_state: crate::detect::AgentState,
    ) {
        let terminal_id = state.workspaces[workspace].tabs[tab].panes[&pane]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .set_detected_state(Some(crate::detect::Agent::Claude), agent_state);
    }

    fn set_pane_open_blocker(
        state: &mut AppState,
        workspace: usize,
        tab: usize,
        pane: crate::layout::PaneId,
    ) {
        let terminal_id = state.workspaces[workspace].tabs[tab].panes[&pane]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .closing_gates = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "gate".into(),
            text: "A latched gate".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
    }

    fn set_pane_open_item(
        state: &mut AppState,
        workspace: usize,
        tab: usize,
        pane: crate::layout::PaneId,
    ) {
        let terminal_id = state.workspaces[workspace].tabs[tab].panes[&pane]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .closing_items = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "Answer".into(),
            text: "Choose a lane".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
    }

    fn remote_blocker(
        host: &str,
        pane_id: &str,
    ) -> std::sync::Arc<crate::ui::RemoteAgentPanelEntry> {
        remote_agent_with_status(host, pane_id, "blocked")
    }

    fn remote_agent_with_status(
        host: &str,
        pane_id: &str,
        agent_status: &str,
    ) -> std::sync::Arc<crate::ui::RemoteAgentPanelEntry> {
        let info = serde_json::from_value(serde_json::json!({
            "terminal_id": format!("terminal-{pane_id}"),
            "name": pane_id,
            "agent": "codex",
            "agent_status": agent_status,
            "workspace_id": "workspace",
            "tab_id": "tab",
            "pane_id": pane_id,
            "focused": false,
            "revision": 1
        }))
        .expect("valid remote agent fixture");
        let snapshot = crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: host.into(),
                target: host.into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: vec![crate::fleet::FleetRow::test_agent_info_row(host, info)],
            }],
            ..crate::fleet::Snapshot::default()
        };
        crate::ui::remote_agent_panel_entries_at(&snapshot, 100, false)
            .into_iter()
            .next()
            .expect("remote blocker entry")
    }

    fn expand_all_workspaces_for_sidebar(state: &mut AppState) {
        for workspace in &state.workspaces {
            state
                .sidebar_presentation
                .expanded_workspace_ids
                .insert(workspace.id.clone());
        }
    }

    fn app_with_blocked_window_fixture() -> App {
        let mut app = app_with_global_window_fixture();
        let blocked_child = app.state.workspaces[1].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        expand_all_workspaces_for_sidebar(&mut app.state);
        set_tab_agent_state(&mut app.state, 0, 1, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut app.state, 1, 0, crate::detect::AgentState::Working);
        set_pane_agent_state(
            &mut app.state,
            1,
            0,
            blocked_child,
            crate::detect::AgentState::Blocked,
        );
        app
    }

    #[test]
    fn next_blocked_window_cycles_canonical_order() {
        let mut app = app_with_blocked_window_fixture();

        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 1)],
        );

        let mut state = app_with_blocked_window_fixture().state;
        assert_headless_window_cycle(
            &mut state,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 1)],
        );
    }

    #[test]
    fn next_blocked_window_visits_every_inbox_pane() {
        let mut app = app_with_global_window_fixture();
        expand_all_workspaces_for_sidebar(&mut app.state);
        for ws_idx in 0..app.state.workspaces.len() {
            for tab_idx in 0..app.state.workspaces[ws_idx].tabs.len() {
                set_tab_agent_state(
                    &mut app.state,
                    ws_idx,
                    tab_idx,
                    crate::detect::AgentState::Blocked,
                );
            }
        }

        let inbox_panes = app
            .state
            .blocked_agents()
            .into_iter()
            .map(|agent| agent.pane_id)
            .collect::<std::collections::HashSet<_>>();
        let cycle_panes = blocked_pane_cycle_in_order(&app.state)
            .into_iter()
            .filter_map(|(target, stops)| match (target, stops) {
                (BlockedPaneTarget::Local { pane_id, .. }, true) => Some(pane_id),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();

        assert!(!inbox_panes.is_empty());
        assert!(inbox_panes.is_subset(&cycle_panes));
    }

    #[test]
    fn next_blocked_window_uses_projection_when_sidebar_tier_is_stale() {
        let mut app = app_with_global_window_fixture();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        set_tab_agent_state(&mut app.state, 0, 0, crate::detect::AgentState::Blocked);
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .supervisor_stale = true;

        let projection = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .agent_projection(&app.state.terminals[&terminal_id]);
        assert!(projection.counts_as_blocked());
        let entry = crate::ui::all_agent_panel_entries(&app.state)
            .into_iter()
            .find(|entry| {
                entry
                    .local_target()
                    .is_some_and(|target| target.pane_id == pane_id)
            })
            .expect("panel entry for test pane");
        assert!(entry.stale);

        assert!(blocked_pane_cycle_in_order(&app.state).iter().any(|(target, stops)| {
            matches!(target, BlockedPaneTarget::Local { pane_id: target_pane, .. } if *target_pane == pane_id)
                && *stops
        }));
    }

    #[test]
    fn next_blocked_window_follows_sidebar_body_order() {
        let mut app = app_with_test_workspaces(&["main", "other", "worktree"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 2, "repo-key");
        app.state.ensure_test_terminals();
        expand_all_workspaces_for_sidebar(&mut app.state);
        for ws_idx in 0..app.state.workspaces.len() {
            set_tab_agent_state(
                &mut app.state,
                ws_idx,
                0,
                crate::detect::AgentState::Blocked,
            );
        }
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::Repo);

        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextBlockedWindow,
            &[(2, 0), (1, 0), (0, 0)],
        );
    }

    #[test]
    fn next_blocked_window_keeps_body_order_when_attention_clears() {
        let mut app = app_with_test_workspaces(&["a", "b", "c"]);
        app.state.ensure_test_terminals();
        expand_all_workspaces_for_sidebar(&mut app.state);
        for ws_idx in 0..3 {
            set_tab_agent_state(
                &mut app.state,
                ws_idx,
                0,
                crate::detect::AgentState::Blocked,
            );
        }
        let panes = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.tabs[0].root_pane)
            .collect::<Vec<_>>();
        app.state.active = Some(0);
        assert_eq!(
            next_blocked_window_target(&app.state),
            Some(BlockedPaneTarget::Local {
                ws_idx: 1,
                tab_idx: 0,
                pane_id: panes[1]
            })
        );

        set_tab_agent_state(&mut app.state, 1, 0, crate::detect::AgentState::Idle);
        app.state.active = Some(1);
        assert_eq!(
            next_blocked_window_target(&app.state),
            Some(BlockedPaneTarget::Local {
                ws_idx: 2,
                tab_idx: 0,
                pane_id: panes[2]
            })
        );

        set_tab_agent_state(&mut app.state, 2, 0, crate::detect::AgentState::Idle);
        app.state.active = Some(2);
        assert_eq!(
            next_blocked_window_target(&app.state),
            Some(BlockedPaneTarget::Local {
                ws_idx: 0,
                tab_idx: 0,
                pane_id: panes[0]
            })
        );
    }

    #[test]
    fn next_blocked_window_places_action_point_sibling_before_later_workspace() {
        let mut app = app_with_test_workspaces(&["mixed", "later"]);
        let visible = app.state.workspaces[0].tabs[0].root_pane;
        let hidden = app.state.workspaces[0].test_split(Direction::Horizontal);
        let later = app.state.workspaces[1].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        expand_all_workspaces_for_sidebar(&mut app.state);
        set_pane_agent_state(
            &mut app.state,
            0,
            0,
            visible,
            crate::detect::AgentState::Blocked,
        );
        set_pane_agent_state(
            &mut app.state,
            0,
            0,
            hidden,
            crate::detect::AgentState::Idle,
        );
        set_pane_open_item(&mut app.state, 0, 0, hidden);
        set_pane_agent_state(
            &mut app.state,
            1,
            0,
            later,
            crate::detect::AgentState::Blocked,
        );
        app.state.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "test.query".into(),
            label: None,
            filter: Some(crate::api::schema::AgentViewFilter::Eq {
                field: crate::api::schema::AgentViewField::Builtin(
                    crate::api::schema::AgentViewBuiltinField::Status,
                ),
                value: crate::api::schema::AgentViewValue::String("blocked".into()),
            }),
            sort: Vec::new(),
        });

        let targets = blocked_pane_cycle_in_order(&app.state)
            .into_iter()
            .filter_map(|(target, needs_attention)| needs_attention.then_some(target))
            .collect::<Vec<_>>();

        assert_eq!(
            targets,
            vec![
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: visible,
                },
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: hidden,
                },
                BlockedPaneTarget::Local {
                    ws_idx: 1,
                    tab_idx: 0,
                    pane_id: later,
                },
            ]
        );
    }

    #[test]
    fn next_blocked_window_follows_strip_for_query_matched_tab_sibling() {
        let mut app = app_with_test_workspaces(&["mixed", "later"]);
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let hidden = app.state.workspaces[0].test_split(Direction::Horizontal);
        let later = app.state.workspaces[1].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        expand_all_workspaces_for_sidebar(&mut app.state);
        for (ws_idx, pane_id, label) in [
            (0, first, "visible first"),
            (0, hidden, "hidden sibling"),
            (1, later, "visible later"),
        ] {
            set_pane_agent_state(
                &mut app.state,
                ws_idx,
                0,
                pane_id,
                crate::detect::AgentState::Blocked,
            );
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .cloned()
                .expect("terminal identity");
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .set_manual_label(label.into());
        }
        app.state.sidebar_work_filter.query = "visible".into();
        app.state.assert_invariants_for_test();

        let targets = blocked_pane_cycle_in_order(&app.state)
            .into_iter()
            .filter_map(|(target, needs_attention)| needs_attention.then_some(target))
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            vec![
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: first,
                },
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: hidden,
                },
                BlockedPaneTarget::Local {
                    ws_idx: 1,
                    tab_idx: 0,
                    pane_id: later,
                },
            ]
        );
        app.state.assert_invariants_for_test();
        app.state.workspaces[0].assert_invariants_for_test();
    }

    #[test]
    fn next_blocked_window_skips_unstarred_target_until_visible_targets() {
        let mut app = app_with_global_window_fixture();
        expand_all_workspaces_for_sidebar(&mut app.state);
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let hidden = app.state.workspaces[0].tabs[1].root_pane;
        let later = app.state.workspaces[1].tabs[0].root_pane;
        for (ws_idx, tab_idx) in [(0, 0), (0, 1), (1, 0)] {
            set_tab_agent_state(
                &mut app.state,
                ws_idx,
                tab_idx,
                crate::detect::AgentState::Blocked,
            );
        }
        app.state.workspaces[0].tabs[0].starred = true;
        app.state.workspaces[1].tabs[0].starred = true;
        app.state.sidebar_starred_only = true;
        app.state.assert_invariants_for_test();

        let targets = blocked_pane_cycle_in_order(&app.state)
            .into_iter()
            .filter_map(|(target, needs_attention)| needs_attention.then_some(target))
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            vec![
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: first,
                },
                BlockedPaneTarget::Local {
                    ws_idx: 1,
                    tab_idx: 0,
                    pane_id: later,
                },
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 1,
                    pane_id: hidden,
                },
            ]
        );
        app.state.assert_invariants_for_test();
        app.state.workspaces[0].assert_invariants_for_test();
    }

    #[test]
    fn next_blocked_window_keeps_blocked_split_at_its_visible_tab_row() {
        let mut app = app_with_test_workspaces(&["mixed", "later"]);
        let working_root = app.state.workspaces[0].tabs[0].root_pane;
        let blocked_split = app.state.workspaces[0].test_split(Direction::Horizontal);
        let later = app.state.workspaces[1].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        expand_all_workspaces_for_sidebar(&mut app.state);
        set_pane_agent_state(
            &mut app.state,
            0,
            0,
            working_root,
            crate::detect::AgentState::Working,
        );
        set_pane_agent_state(
            &mut app.state,
            0,
            0,
            blocked_split,
            crate::detect::AgentState::Blocked,
        );
        set_pane_agent_state(
            &mut app.state,
            1,
            0,
            later,
            crate::detect::AgentState::Blocked,
        );
        app.state.assert_invariants_for_test();

        let targets = blocked_pane_cycle_in_order(&app.state)
            .into_iter()
            .filter_map(|(target, needs_attention)| needs_attention.then_some(target))
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            vec![
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: blocked_split,
                },
                BlockedPaneTarget::Local {
                    ws_idx: 1,
                    tab_idx: 0,
                    pane_id: later,
                },
            ]
        );
        app.state.assert_invariants_for_test();
        app.state.workspaces[0].assert_invariants_for_test();
    }

    #[test]
    fn next_blocked_window_visits_yellow_and_red_in_sidebar_order() {
        let configure = |state: &mut AppState| {
            expand_all_workspaces_for_sidebar(state);
            let yellow = state.workspaces[0].tabs[1].root_pane;
            let red = state.workspaces[1].tabs[0].root_pane;
            set_tab_agent_state(state, 0, 1, crate::detect::AgentState::Blocked);
            set_pane_open_item(state, 0, 1, yellow);
            set_tab_agent_state(state, 1, 0, crate::detect::AgentState::Idle);
            set_pane_open_blocker(state, 1, 0, red);
        };

        let mut app = app_with_global_window_fixture();
        configure(&mut app.state);
        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 1)],
        );

        let mut state = app_with_global_window_fixture().state;
        configure(&mut state);
        assert_headless_window_cycle(
            &mut state,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 1)],
        );
    }

    #[test]
    fn next_blocked_window_visits_working_pane_with_blocker_and_idle_blocked_pane() {
        let mut app = app_with_global_window_fixture();
        expand_all_workspaces_for_sidebar(&mut app.state);
        let working_blocker = app.state.workspaces[0].tabs[1].root_pane;
        let idle_blocker = app.state.workspaces[1].tabs[0].root_pane;

        set_tab_agent_state(&mut app.state, 0, 0, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut app.state, 0, 1, crate::detect::AgentState::Working);
        set_pane_open_blocker(&mut app.state, 0, 1, working_blocker);
        set_tab_agent_state(&mut app.state, 1, 0, crate::detect::AgentState::Idle);
        set_pane_open_blocker(&mut app.state, 1, 0, idle_blocker);

        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 0)],
        );

        let mut state = app_with_global_window_fixture().state;
        expand_all_workspaces_for_sidebar(&mut state);
        let working_blocker = state.workspaces[0].tabs[1].root_pane;
        let idle_blocker = state.workspaces[1].tabs[0].root_pane;

        set_tab_agent_state(&mut state, 0, 0, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut state, 0, 1, crate::detect::AgentState::Working);
        set_pane_open_blocker(&mut state, 0, 1, working_blocker);
        set_tab_agent_state(&mut state, 1, 0, crate::detect::AgentState::Idle);
        set_pane_open_blocker(&mut state, 1, 0, idle_blocker);

        assert_headless_window_cycle(
            &mut state,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 0)],
        );
    }

    #[test]
    fn next_blocked_window_excludes_future_snoozed_remote_rows() {
        let remote_info = |pane_id: &str, snoozed_until: Option<u64>| {
            let mut value = serde_json::json!({
                "terminal_id": format!("terminal-{pane_id}"),
                "name": pane_id,
                "agent": "codex",
                "agent_status": "blocked",
                "workspace_id": "workspace",
                "tab_id": "tab",
                "pane_id": pane_id,
                "focused": false,
                "revision": 1
            });
            if let Some(deadline) = snoozed_until {
                value["snoozed_until"] = serde_json::json!(deadline);
            }
            serde_json::from_value(value).expect("valid remote agent fixture")
        };
        let snapshot = crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: "remote".into(),
                target: "remote".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: vec![
                    crate::fleet::FleetRow::test_agent_info_row(
                        "remote",
                        remote_info("visible", None),
                    ),
                    crate::fleet::FleetRow::test_agent_info_row(
                        "remote",
                        remote_info("snoozed", Some(200)),
                    ),
                ],
            }],
            ..crate::fleet::Snapshot::default()
        };
        let mut state = AppState::test_new();
        state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        state.remote_agent_panel_entries =
            crate::ui::remote_agent_panel_entries_at(&snapshot, 100, false);
        state.view_observed_unix_s = 100;
        state.collapsed_sidebar_groups.remove("repo:Fleet");

        let targets = blocked_pane_cycle_in_order(&state)
            .into_iter()
            .filter_map(|(target, needs_attention)| needs_attention.then_some(target))
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            vec![BlockedPaneTarget::Remote(
                crate::api::schema::AgentRef::new("remote", "visible")
                    .expect("valid visible remote reference")
            )]
        );
    }

    #[test]
    fn next_blocked_window_reaches_remote_row_after_device_group_removal() {
        let mut state = AppState::test_new();
        state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        let remote = remote_blocker("ub2", "blocked-pane");
        let agent_ref = remote.agent_ref.clone();
        state.remote_agent_panel_entries = vec![remote];
        state.collapsed_sidebar_groups.insert("repo:Fleet".into());

        let rows = crate::ui::sidebar_rows(&state);
        assert!(rows.iter().any(|row| matches!(
            row,
            crate::ui::SidebarRow::NeedsYou {
                target: crate::ui::sidebar::NeedsYouTarget::Remote(target), ..
            } if *target == agent_ref
        )));
        assert!(rows.iter().any(|row| matches!(
            row,
            crate::ui::SidebarRow::RemoteAgent { entry, .. }
                if entry.agent_ref == agent_ref
        )));
        assert_eq!(
            next_blocked_window_target(&state),
            Some(BlockedPaneTarget::Remote(agent_ref))
        );
    }

    #[test]
    fn fleet_workspace_ac7_next_blocked_window_focuses_remote_agent_tab() {
        let mut config = Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "ub2".into(),
            target: "remote-ub2".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.workspaces = vec![Workspace::test_new("local"), Workspace::test_new("fleet")];
        app.state.workspaces[1].is_fleet = true;
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let remote = remote_blocker("ub2", "blocked-agent");
        app.state.remote_agent_panel_entries = vec![remote];
        assert_eq!(next_blocked_window_target(&app.state), None);
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        app.state.fleet_snapshot.hosts = vec![crate::fleet::HostSnapshot {
            name: "ub2".into(),
            target: "remote-ub2".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }];
        let argv = crate::fleet::agent_attach_argv_from_config(
            &config.remote.fleet.hosts[0],
            "blocked-agent",
        )
        .expect("configured remote attach argv");
        let fleet_tab = &app.state.workspaces[1].tabs[0];
        let fleet_root_pane = fleet_tab.root_pane;
        let fleet_terminal_id = fleet_tab
            .terminal_id(fleet_tab.root_pane)
            .expect("fleet terminal id")
            .clone();
        app.state
            .terminals
            .get_mut(&fleet_terminal_id)
            .expect("fleet terminal")
            .launch_argv = Some(argv);
        let local_tab_count = app.state.workspaces[0].tabs.len();
        let terminal_count = app.state.terminals.len();

        app.focus_next_blocked_window();

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab_index(), 0);
        assert_eq!(
            app.state.workspaces[1].focused_pane_id(),
            Some(fleet_root_pane)
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), local_tab_count);
        assert_eq!(app.state.terminals.len(), terminal_count);
        assert_eq!(
            app.state.sidebar_selected_remote_agent,
            Some(
                crate::api::schema::AgentRef::new("ub2", "blocked-agent")
                    .expect("valid remote reference")
            )
        );
    }

    #[test]
    fn fleet_workspace_ac8_blocked_filter_lists_and_cycles_only_blocked_fleet_agents() {
        let mut config = Config::default();
        config.remote.fleet.hosts = vec![crate::config::FleetHostConfig {
            name: "ub2".into(),
            target: "remote-ub2".into(),
            ..Default::default()
        }];
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        let mut fleet = Workspace::test_new("fleet");
        fleet.is_fleet = true;
        fleet.test_add_tab(Some("blocked-two"));
        app.state.workspaces = vec![Workspace::test_new("local"), fleet];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.remote_agent_panel_entries = vec![
            remote_blocker("ub2", "blocked-one"),
            remote_agent_with_status("ub2", "working", "working"),
            remote_blocker("ub2", "blocked-two"),
        ];
        app.state.fleet_snapshot.hosts = vec![crate::fleet::HostSnapshot {
            name: "ub2".into(),
            target: "remote-ub2".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }];
        let fleet_terminal_ids = app.state.workspaces[1]
            .tabs
            .iter()
            .map(|tab| {
                tab.terminal_id(tab.root_pane)
                    .expect("fleet tab terminal")
                    .clone()
            })
            .collect::<Vec<_>>();
        for (tab_idx, agent) in [(0, "blocked-one"), (1, "blocked-two")] {
            let argv =
                crate::fleet::agent_attach_argv_from_config(&config.remote.fleet.hosts[0], agent)
                    .expect("configured remote attach argv");
            app.state
                .terminals
                .get_mut(&fleet_terminal_ids[tab_idx])
                .expect("fleet terminal")
                .launch_argv = Some(argv);
        }
        let fleet_key = if app.state.sidebar_sections_layout {
            "sections:Fleet"
        } else {
            "repo:Fleet"
        };
        app.state.collapsed_sidebar_groups.remove(fleet_key);
        execute_navigate_action_in_context(
            &mut app.state,
            &mut app.terminal_runtimes,
            NavigateAction::ToggleBlockedFilter,
            ActionContext::Prefix,
        );
        assert!(app.state.blocked_filter);

        let visible_agents = crate::ui::sidebar_rows(&app.state)
            .into_iter()
            .filter_map(|row| match row {
                crate::ui::SidebarRow::RemoteAgent { entry, .. } => {
                    Some(entry.agent_ref.agent.clone())
                }
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            visible_agents,
            ["blocked-one".to_string(), "blocked-two".to_string()]
                .into_iter()
                .collect()
        );
        let blocked_targets = blocked_pane_cycle_in_order(&app.state)
            .into_iter()
            .filter_map(|(target, blocked)| blocked.then_some(target))
            .collect::<Vec<_>>();
        assert_eq!(
            blocked_targets,
            vec![
                BlockedPaneTarget::Remote(
                    crate::api::schema::AgentRef::new("ub2", "blocked-one")
                        .expect("valid remote reference")
                ),
                BlockedPaneTarget::Remote(
                    crate::api::schema::AgentRef::new("ub2", "blocked-two")
                        .expect("valid remote reference")
                ),
            ]
        );

        app.focus_next_blocked_window();
        assert_eq!(
            app.state
                .sidebar_selected_remote_agent
                .as_ref()
                .unwrap()
                .agent,
            "blocked-one"
        );
        app.focus_next_blocked_window();
        assert_eq!(
            app.state
                .sidebar_selected_remote_agent
                .as_ref()
                .unwrap()
                .agent,
            "blocked-two"
        );
    }

    #[test]
    fn next_blocked_window_visits_each_strip_target_once_per_lap() {
        let mut state = AppState::test_new();
        state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        state.workspaces = vec![Workspace::test_new("local")];
        state.ensure_test_terminals();
        state.active = Some(0);
        let local_pane = state.workspaces[0].tabs[0].root_pane;
        set_pane_agent_state(
            &mut state,
            0,
            0,
            local_pane,
            crate::detect::AgentState::Blocked,
        );
        let remote = remote_blocker("ub2", "blocked-pane");
        let agent_ref = remote.agent_ref.clone();
        state.remote_agent_panel_entries = vec![remote];
        state.collapsed_sidebar_groups.remove("repo:Fleet");

        let rows = crate::ui::sidebar_rows(&state);
        assert!(rows.iter().any(|row| matches!(
            row,
            crate::ui::SidebarRow::RemoteAgent { entry, .. }
                if entry.agent_ref == agent_ref
        )));
        let targets = blocked_pane_cycle_in_order(&state)
            .into_iter()
            .filter_map(|(target, needs_attention)| needs_attention.then_some(target))
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            vec![
                BlockedPaneTarget::Local {
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id: local_pane,
                },
                BlockedPaneTarget::Remote(agent_ref.clone()),
            ]
        );

        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextBlockedWindow,
            ActionContext::Prefix,
        );
        assert_eq!(state.sidebar_selected_remote_agent, Some(agent_ref.clone()));
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextBlockedWindow,
            ActionContext::Prefix,
        );
        assert!(state.sidebar_selected_remote_agent.is_none());
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(local_pane));
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextBlockedWindow,
            ActionContext::Prefix,
        );
        assert_eq!(state.sidebar_selected_remote_agent, Some(agent_ref));
    }

    #[test]
    fn next_blocked_window_selects_remote_row_without_focusing_a_local_pane() {
        let mut app = app_with_global_window_fixture();
        app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        let original_window = active_window(&app.state);
        let original_pane = app.state.workspaces[original_window.0]
            .focused_pane_id()
            .expect("focused local pane");
        let mut remote = crate::ui::all_agent_panel_entries(&app.state)
            .into_iter()
            .next()
            .expect("agent panel fixture");
        remote.state = crate::detect::AgentState::Idle;
        remote.open_blockers = true;
        remote.attention_tier = Some(crate::terminal::state::AttentionTier::Blocked);
        let agent_ref = crate::api::schema::AgentRef::new("ub2", "pane/with/slash")
            .expect("valid remote agent reference");
        app.state.remote_agent_panel_entries = vec![std::sync::Arc::new(
            crate::ui::RemoteAgentPanelEntry::new(agent_ref.clone(), remote),
        )];
        app.state.collapsed_sidebar_groups.remove("repo:Fleet");
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 106, 30));
        app.execute_tui_navigate_action(NavigateAction::NextBlockedWindow, ActionContext::Prefix);

        assert_eq!(active_window(&app.state), original_window);
        assert_eq!(
            app.state.workspaces[original_window.0].focused_pane_id(),
            Some(original_pane)
        );
        assert_eq!(
            app.state
                .sidebar_selected_remote_agent
                .as_ref()
                .map(ToString::to_string),
            Some("ub2::pane/with/slash".into())
        );
        assert!(
            crate::ui::compute_remote_agent_row_areas(&app.state, app.state.view.sidebar_rect)
                .iter()
                .any(|area| area.agent_ref == agent_ref)
        );

        assert!(!app
            .state
            .focus_pane_in_workspace(original_window.0, original_pane));
        assert!(app.state.sidebar_selected_remote_agent.is_none());
    }

    #[test]
    fn next_blocked_window_reaches_remote_blockers_after_machine_filter_removal() {
        for sections in [false, true] {
            let mut app = app_with_global_window_fixture();
            app.state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
            app.state.sidebar_sections_layout = sections;
            let mut remote = crate::ui::all_agent_panel_entries(&app.state)
                .into_iter()
                .next()
                .expect("agent panel fixture");
            remote.state = crate::detect::AgentState::Idle;
            remote.open_blockers = true;
            remote.attention_tier = Some(crate::terminal::state::AttentionTier::Blocked);
            let agent_ref = crate::api::schema::AgentRef::new("ub1", "blocked")
                .expect("valid remote agent reference");
            app.state.remote_agent_panel_entries = vec![std::sync::Arc::new(
                crate::ui::RemoteAgentPanelEntry::new(agent_ref.clone(), remote),
            )];
            let reaches_remote = |state: &AppState| {
                blocked_pane_cycle_in_order(state).iter().any(|(target, _)| {
                    matches!(target, BlockedPaneTarget::Remote(found) if *found == agent_ref)
                })
            };

            app.state.skip_collapsed_cycle = false;
            assert!(reaches_remote(&app.state), "sections={sections}");
            // Compact layout keeps the blocker in Needs you; sections layout hides the collapsed device.
            app.state.skip_collapsed_cycle = true;
            assert_eq!(reaches_remote(&app.state), !sections, "sections={sections}");
        }
    }

    #[test]
    fn next_blocked_window_default_binding_dispatches_action() {
        let state = state_with_workspaces(&["test"]);

        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('b'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::NextBlockedWindow)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('b'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::ToggleSidebar)
        );
    }

    #[test]
    fn sidebar_cycle_group_mode_binding_dispatches_and_cycles() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.sidebar_cycle_group_mode = crate::config::ActionKeybinds::prefix("g");

        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CycleSidebarGroupMode)
        );
        execute_navigate_action(&mut state, NavigateAction::CycleSidebarGroupMode);
        assert_eq!(
            state.sidebar_group_mode,
            crate::app::state::SidebarGroupMode::Spaces
        );
        assert_eq!(
            state.take_sidebar_group_mode_persistence_request(),
            Some(crate::app::state::SidebarGroupMode::Spaces)
        );
        execute_navigate_action(&mut state, NavigateAction::CycleSidebarGroupMode);
        assert_eq!(
            state.sidebar_group_mode,
            crate::app::state::SidebarGroupMode::LinearTeam
        );
    }

    #[test]
    fn sidebar_refresh_binding_dispatches_and_sets_the_request() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.sidebar_refresh = crate::config::ActionKeybinds::prefix("u");

        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('u'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::RefreshSidebar)
        );
        execute_navigate_action(&mut state, NavigateAction::RefreshSidebar);
        assert!(state.sidebar_refreshing);
        assert!(state.sidebar_refresh_requested);
    }

    fn state_with_git_repo(in_git_repo: bool) -> AppState {
        let mut state = state_with_workspaces(&["test"]);
        let cwd = std::path::PathBuf::from("/repo/herdr");
        state.status_focused_cwd = Some(cwd.clone());
        state.git_root_for_cwd.insert(
            cwd.clone(),
            in_git_repo.then_some(std::path::PathBuf::from("/repo/herdr")),
        );
        state
    }

    #[test]
    fn git_palette_actions_request_the_matching_git_action_in_a_repo() {
        for (action, expected) in [
            (NavigateAction::GitPull, crate::app::state::GitAction::Pull),
            (
                NavigateAction::GitCommit,
                crate::app::state::GitAction::Commit,
            ),
            (NavigateAction::GitPush, crate::app::state::GitAction::Push),
            (
                NavigateAction::GitCreatePr,
                crate::app::state::GitAction::CreatePr,
            ),
        ] {
            let mut state = state_with_git_repo(true);
            execute_navigate_action(&mut state, action);
            assert_eq!(state.request_git_action, Some(expected));
            assert_eq!(state.server_mode(), Mode::Terminal);
        }
    }

    #[test]
    fn git_palette_actions_are_no_ops_outside_a_repo() {
        for action in [
            NavigateAction::GitPull,
            NavigateAction::GitCommit,
            NavigateAction::GitPush,
            NavigateAction::GitCreatePr,
        ] {
            let mut state = state_with_git_repo(false);
            execute_navigate_action(&mut state, action);
            assert_eq!(state.request_git_action, None);
            assert_eq!(state.server_mode(), Mode::Navigate);
        }
    }

    #[test]
    fn focus_sidebar_action_focuses_and_expands_the_sidebar() {
        let mut state = state_with_workspaces(&["test"]);
        state.sidebar_collapsed = true;

        execute_navigate_action(&mut state, NavigateAction::FocusSidebar);

        assert!(state.sidebar_focused);
        assert!(!state.sidebar_collapsed);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn next_blocked_window_handles_no_match_and_nonblocked_current() {
        let mut app = app_with_global_window_fixture();
        app.state.switch_tab(1);
        app.execute_tui_navigate_action(NavigateAction::NextBlockedWindow, ActionContext::Prefix);
        assert_eq!(active_window(&app.state), (0, 1));

        set_tab_agent_state(&mut app.state, 0, 0, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut app.state, 1, 1, crate::detect::AgentState::Blocked);
        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextBlockedWindow,
            &[(1, 1), (0, 0)],
        );

        let mut state = app_with_global_window_fixture().state;
        state.switch_tab(1);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextBlockedWindow,
            ActionContext::Prefix,
        );
        assert_eq!(active_window(&state), (0, 1));

        set_tab_agent_state(&mut state, 0, 0, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut state, 1, 1, crate::detect::AgentState::Blocked);
        assert_headless_window_cycle(
            &mut state,
            NavigateAction::NextBlockedWindow,
            &[(1, 1), (0, 0)],
        );
    }

    fn temporary_checkout(name: &str, origin: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-symphony-{name}-{}-{unique}",
            std::process::id()
        ));
        let checkout = root.join(name);
        std::fs::create_dir_all(&checkout).expect("create test checkout");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("run git init");
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["remote", "add", "origin", origin])
            .current_dir(&checkout)
            .status()
            .expect("add git origin");
        assert!(status.success());
        checkout
    }

    fn point_workspace_at(app: &mut App, workspace: usize, cwd: &std::path::Path) {
        let root_pane = app.state.workspaces[workspace].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[workspace].tabs[0]
            .terminal_id(root_pane)
            .expect("root terminal")
            .clone();
        app.state.workspaces[workspace].identity_cwd = cwd.to_path_buf();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .cwd = cwd.to_path_buf();
    }

    fn set_symphony_workflow(app: &mut App, repo: &str) {
        app.state.symphony_detail = Some(crate::app::state::SymphonyDetail {
            snapshot: crate::symphony::Snapshot {
                workflows: vec![crate::symphony::Workflow {
                    workflow_id: "symphony-MAT-138".to_string(),
                    run_id: "run".to_string(),
                    name: "Temporal blocker dashboard".to_string(),
                    phase: "runFlowStep".to_string(),
                    wait: Some("plan-sign-off".to_string()),
                    started_at: None,
                    ticket: Some("MAT-138".to_string()),
                    repo: Some(repo.to_string()),
                    pr: Some("https://github.com/matthias-scale/herdr/pull/1".to_string()),
                    receipts: Some("/receipts".to_string()),
                }],
                unavailable: None,
                polled: true,
            },
            selected: 0,
            observed_at: std::time::SystemTime::now(),
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prefix_pass_through_retires_blocked_hook_authority_after_forwarding() {
        let mut app = app_with_test_workspaces(&["test"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
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
        app.state.set_server_mode(Mode::Prefix);

        app.handle_prefix_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ));

        assert!(rx.try_recv().is_ok());
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );
        assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
    }

    /// AC6: a successfully forwarded prefix pass-through key records the human draft.
    #[tokio::test(flavor = "current_thread")]
    async fn prefix_pass_through_records_a_human_draft() {
        let mut app = app_with_test_workspaces(&["test"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
        app.state.prefix_code = KeyCode::Char('x');
        app.state.prefix_mods = KeyModifiers::empty();
        app.state.set_server_mode(Mode::Prefix);

        app.handle_prefix_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ));

        assert!(rx.try_recv().is_ok());
        assert_eq!(app.state.pending_human_drafts[&pane_id], "x");
    }

    #[test]
    fn prefix_ctrl_h_opens_and_escape_closes_run_history_detail() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.set_server_mode(Mode::Prefix);

        app.handle_prefix_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::CONTROL));

        assert!(app.state.loop_run_history_detail.is_some());

        assert!(
            app.handle_loop_run_history_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty(),))
        );

        assert!(app.state.loop_run_history_detail.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn symphony_enter_opens_interactive_tab_in_matching_checkout() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.default_shell = crate::app::api::test_support::exiting_test_command().into();
        let cwd = temporary_checkout(
            "mat-138-symphony-service",
            "git@github.com:owner-a/mat-138-symphony-service.git",
        );
        point_workspace_at(&mut app, 0, &cwd);
        set_symphony_workflow(&mut app, "owner-a/mat-138-symphony-service");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.symphony_detail.is_none());
        let created = &app.state.workspaces[0].tabs[1];
        let terminal_id = created.terminal_id(created.root_pane).unwrap();
        assert_eq!(app.state.terminals[terminal_id].cwd, cwd);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        std::fs::remove_dir_all(cwd.parent().expect("checkout parent"))
            .expect("remove test checkout");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn symphony_enter_accepts_matching_origin_with_custom_checkout_basename() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.default_shell = crate::app::api::test_support::exiting_test_command().into();
        let cwd = temporary_checkout("custom-worktree-path", "git@github.com:owner-a/service.git");
        point_workspace_at(&mut app, 0, &cwd);
        set_symphony_workflow(&mut app, "owner-a/service");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.symphony_detail.is_none());
        let created = &app.state.workspaces[0].tabs[1];
        let terminal_id = created.terminal_id(created.root_pane).unwrap();
        assert_eq!(app.state.terminals[terminal_id].cwd, cwd);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        std::fs::remove_dir_all(cwd.parent().expect("checkout parent"))
            .expect("remove test checkout");
    }

    #[test]
    fn symphony_enter_rejects_same_basename_with_different_owner() {
        let mut app = app_with_test_workspaces(&["test"]);
        let cwd = temporary_checkout(
            "mat-138-symphony-collision",
            "git@github.com:owner-b/mat-138-symphony-collision.git",
        );
        point_workspace_at(&mut app, 0, &cwd);
        set_symphony_workflow(&mut app, "owner-a/mat-138-symphony-collision");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(app.state.symphony_detail.is_some());
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("Symphony checkout origin mismatch for owner-a/mat-138-symphony-collision")
        );
        std::fs::remove_dir_all(cwd.parent().expect("checkout parent"))
            .expect("remove test checkout");
    }

    #[test]
    fn symphony_enter_rejects_hostile_repository_name() {
        let mut app = app_with_test_workspaces(&["test"]);
        set_symphony_workflow(&mut app, "..");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(app.state.symphony_detail.is_some());
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("Invalid Symphony repository name: ..")
        );
    }

    #[test]
    fn toggle_tab_prio_flips_flag_and_marks_session_dirty() {
        let mut app = app_with_test_workspaces(&["one"]);
        app.no_session = false;
        app.state.session_dirty = false;
        app.state.session_dirty_revision = 0;

        app.execute_tui_navigate_action(NavigateAction::ToggleTabPrio, ActionContext::Direct);
        assert!(app.state.workspaces[0].tabs[0].prio);
        assert!(app.state.session_dirty);
        assert_eq!(app.state.session_dirty_revision, 1);

        app.execute_tui_navigate_action(NavigateAction::ToggleTabPrio, ActionContext::Direct);
        assert!(!app.state.workspaces[0].tabs[0].prio);
        assert_eq!(app.state.session_dirty_revision, 2);
    }

    fn add_multiple_work_links(app: &mut App) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/2".into()]),
                ..Default::default()
            })
            .unwrap();
    }

    fn register_review_test_context(
        state: &mut AppState,
        ws_idx: usize,
        role: crate::work_context::PaneWorkRole,
        pr_number: u64,
    ) {
        let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
        let terminal_id = state.workspaces[ws_idx]
            .terminal_id(pane_id)
            .expect("root terminal")
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec![format!(
                    "https://github.com/owner/repo/pull/{pr_number}"
                )]),
                role: Some(role),
                ..Default::default()
            })
            .expect("valid work context");
    }

    #[test]
    fn selecting_the_editor_surface_retries_after_the_editor_exited() {
        let mut state = app_with_test_workspaces(&["one"]).state;
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Prefix);
        let agent_pane_id = state.workspaces[0].focused_pane_id().expect("focused pane");
        let terminal_id = state.workspaces[0]
            .terminal_id(agent_pane_id)
            .expect("agent terminal")
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .detected_agent = Some(crate::detect::Agent::Codex);
        state
            .dock_editor_errors
            .insert(agent_pane_id, "editor exited".to_string());
        state.dock_open_surfaces = vec![
            crate::app::DockSurface::Home,
            crate::app::DockSurface::Editor,
        ];
        state.dock_tab = Some(crate::app::DockSurface::Home);

        for _ in 0..8 {
            if state.dock_tab == Some(crate::app::DockSurface::Editor) {
                break;
            }
            execute_navigate_action_in_context(
                &mut state,
                &mut terminal_runtimes,
                NavigateAction::NextDockTab,
                ActionContext::Prefix,
            );
        }

        assert_eq!(state.dock_tab, Some(crate::app::DockSurface::Editor));
        assert!(state.dock_editor_focused);
        assert!(!state.dock_editor_errors.contains_key(&agent_pane_id));
    }

    #[test]
    fn dock_key_actions_cycle_tabs_and_toggle_the_dock() {
        let mut state = app_with_test_workspaces(&["one"]).state;
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Prefix);
        state.dock_open_surfaces = vec![
            crate::app::DockSurface::Home,
            crate::app::DockSurface::Editor,
        ];
        state.dock_tab = Some(crate::app::DockSurface::Home);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::Tickets
        );
        assert_eq!(state.dock_tab, Some(crate::app::DockSurface::Home));
        assert!(state.dock_home_focused);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::XPolls
        );
        assert_eq!(state.dock_tab, Some(crate::app::DockSurface::Home));

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(state.dock_tab, Some(crate::app::DockSurface::Editor));
        assert!(state.dock_editor_focused);
        assert!(!state.dock_home_focused);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::PreviousDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(state.dock_tab, Some(crate::app::DockSurface::Home));
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::XPolls
        );
        assert!(state.dock_home_focused);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::PreviousDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::Tickets
        );
        assert_eq!(state.dock_tab, Some(crate::app::DockSurface::Home));

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::PreviousDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::Prs
        );
        assert_eq!(state.dock_tab, Some(crate::app::DockSurface::Home));

        state.dock_collapsed = true;
        state.session_dirty = false;
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::ToggleDock,
            ActionContext::Prefix,
        );
        assert!(!state.dock_collapsed);
        assert!(
            !state.session_dirty,
            "client-local dock presentation must not dirty shared session state"
        );
    }

    #[test]
    fn next_review_agent_cycles_canonical_bindings_and_wraps() {
        let mut state = app_with_test_workspaces(&["review-a", "ship", "review-b"]).state;
        register_review_test_context(&mut state, 0, crate::work_context::PaneWorkRole::Review, 10);
        register_review_test_context(&mut state, 1, crate::work_context::PaneWorkRole::Ship, 20);
        register_review_test_context(&mut state, 2, crate::work_context::PaneWorkRole::Review, 30);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextReviewAgent,
            ActionContext::Prefix,
        );
        assert_eq!(state.active, Some(2));

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextReviewAgent,
            ActionContext::Prefix,
        );
        assert_eq!(state.active, Some(0));

        let ship_pane = state.workspaces[1].tabs[0].root_pane;
        assert!(state.focus_pane_in_workspace(1, ship_pane));
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextReviewAgent,
            ActionContext::Prefix,
        );
        assert_eq!(state.active, Some(0));
    }

    #[test]
    fn next_review_agent_is_a_noop_without_review_bindings() {
        let mut state = app_with_test_workspaces(&["ship"]).state;
        register_review_test_context(&mut state, 0, crate::work_context::PaneWorkRole::Ship, 10);
        let focused_before = state.current_pane_focus_target();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextReviewAgent,
            ActionContext::Prefix,
        );

        assert_eq!(state.current_pane_focus_target(), focused_before);
    }

    #[test]
    fn next_review_agent_default_avoids_reload_config_collision() {
        let state = app_with_test_workspaces(&["one"]).state;

        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::NextReviewAgent)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('r'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::ReloadConfig)
        );
    }

    #[test]
    fn default_ticket_keybinding_opens_ticket_projection() {
        let mut state = app_with_test_workspaces(&["one"]).state;
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::OpenTicketView)
        );
        let mut runtimes = TerminalRuntimeRegistry::new();
        execute_navigate_action_in_context(
            &mut state,
            &mut runtimes,
            NavigateAction::OpenTicketView,
            ActionContext::Prefix,
        );
        assert!(state
            .work_view
            .as_ref()
            .is_some_and(|view| { view.projection == crate::app::state::WorkProjection::Tickets }));
    }

    #[test]
    fn configured_missive_keybinding_opens_missive_projection() {
        let mut state = app_with_test_workspaces(&["one"]).state;
        // Unbound by default: the Missive view released `c` to the spawn flow,
        // so only a configured binding reaches it.
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('c'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            None
        );
        state.keybinds.missive = crate::config::ActionKeybinds::prefix("m");
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::OpenMissiveView)
        );
        let mut runtimes = TerminalRuntimeRegistry::new();
        execute_navigate_action_in_context(
            &mut state,
            &mut runtimes,
            NavigateAction::OpenMissiveView,
            ActionContext::Prefix,
        );
        assert!(state
            .work_view
            .as_ref()
            .is_some_and(|view| { view.projection == crate::app::state::WorkProjection::Missive }));
    }

    #[test]
    fn ac4_default_work_link_keybindings_map_to_distinct_prefix_actions() {
        let state = app_with_test_workspaces(&["one"]).state;
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('e'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::ToggleDock)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('['), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::PreviousDockTab)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char(']'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::NextDockTab)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('u'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::OpenWorkUrl)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('U'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkUrl)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkTicket)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('u'), KeyModifiers::ALT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkPr)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(
                    KeyCode::Char('u'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkPreview)
        );
        // The info panel's old `prefix+i` slot now collapses every repo group
        // except the focused pane's.
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('i'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::FocusOwningRepoGroup)
        );
    }

    #[test]
    fn ac4_work_link_resolver_and_clipboard_use_only_active_focused_pane() {
        let mut app = app_with_test_workspaces(&["active", "selected"]);
        app.state.selected = 1;
        let focused = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(focused);

        let root = app.state.workspaces[0].tabs[0].root_pane;
        let selected = app.state.workspaces[1].tabs[0].root_pane;
        for (ws_idx, pane, ticket, pr) in [
            (
                0,
                root,
                None,
                Some("https://github.com/ogulcancelik/herdr/pull/1"),
            ),
            (
                0,
                focused,
                Some("SCA-42"),
                Some("https://github.com/ogulcancelik/herdr/pull/4"),
            ),
            (1, selected, Some("SCA-999"), None),
        ] {
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane)
                .cloned()
                .unwrap();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    ticket_ids: ticket.map(|ticket| vec![ticket.into()]),
                    pr_urls: pr.map(|pr| vec![pr.into()]),
                    ..Default::default()
                })
                .unwrap();
        }

        assert_eq!(
            focused_work_url(&app.state).as_deref(),
            Some("https://linear.app/scalable/issue/SCA-42")
        );
        app.execute_tui_navigate_action(NavigateAction::CopyWorkUrl, ActionContext::Prefix);
        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://linear.app/scalable/issue/SCA-42")
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn ac4_missing_work_context_is_a_nonfatal_notice_without_clipboard_event() {
        let mut app = app_with_test_workspaces(&["one"]);
        app.execute_tui_navigate_action(NavigateAction::CopyWorkUrl, ActionContext::Prefix);

        assert!(app.event_rx.try_recv().is_err());
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("focused pane has no work link")
        );
    }

    #[test]
    fn ac25_granular_work_copy_actions_use_effective_focused_context() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["mat-231".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/231".into()]),
                ..Default::default()
            })
            .unwrap();
        terminal
            .replace_hook_work_context(crate::work_context::PaneWorkContext {
                preview_urls: vec!["https://preview-231.vercel.app".into()],
                ..Default::default()
            })
            .unwrap();

        for (action, expected) in [
            (NavigateAction::CopyWorkTicket, b"MAT-231".as_slice()),
            (
                NavigateAction::CopyWorkPr,
                b"https://github.com/o/r/pull/231".as_slice(),
            ),
            (
                NavigateAction::CopyWorkPreview,
                b"https://preview-231.vercel.app".as_slice(),
            ),
        ] {
            app.execute_tui_navigate_action(action, ActionContext::Prefix);
            match app.event_rx.try_recv().expect("clipboard event") {
                crate::events::AppEvent::ClipboardWrite { content } => {
                    assert_eq!(content, expected)
                }
                event => panic!("unexpected event: {event:?}"),
            }
        }
    }

    #[test]
    fn ac25_copy_work_preview_hook_tier_precedes_restored_manual_and_git_tiers() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .restore_work_context_with_tiers(
                crate::work_context::PaneWorkContext::default(),
                Some(crate::work_context::PaneWorkContextTiers {
                    manual: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://manual.vercel.app".into()],
                        ..Default::default()
                    },
                    hook_turn: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://hook.vercel.app".into()],
                        ..Default::default()
                    },
                    git_observation: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://git.vercel.app".into()],
                        ..Default::default()
                    },
                    restored_fallback: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://fallback.vercel.app".into()],
                        ..Default::default()
                    },
                }),
            )
            .unwrap();

        app.execute_tui_navigate_action(NavigateAction::CopyWorkPreview, ActionContext::Prefix);
        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://hook.vercel.app")
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn ac25_granular_work_copy_actions_are_safe_noops_when_missing() {
        for (action, notice) in [
            (
                NavigateAction::CopyWorkTicket,
                "focused pane has no work ticket",
            ),
            (
                NavigateAction::CopyWorkPr,
                "focused pane has no pull request",
            ),
            (
                NavigateAction::CopyWorkPreview,
                "focused pane has no preview URL",
            ),
        ] {
            let mut app = app_with_test_workspaces(&["one"]);
            app.execute_tui_navigate_action(action, ActionContext::Prefix);
            assert!(app.event_rx.try_recv().is_err());
            assert_eq!(
                app.state
                    .copy_feedback
                    .as_ref()
                    .map(|feedback| feedback.message.as_str()),
                Some(notice)
            );
        }
    }

    #[test]
    fn ac26_link_picker_inherits_copy_action_and_selects_snapshot_entry() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/2".into()]),
                ..Default::default()
            })
            .unwrap();

        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Copy
        );

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty()));
        match app.event_rx.try_recv().expect("picker clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://github.com/o/r/pull/2")
            }
            event => panic!("unexpected event: {event:?}"),
        }
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn ac26_legacy_prefix_binding_dispatches_to_picker_action() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        app.state.set_server_mode(Mode::Prefix);
        app.handle_prefix_key(TerminalKey::new(KeyCode::Char('U'), KeyModifiers::SHIFT));

        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Copy
        );
    }

    #[test]
    fn ac26_navigate_mode_work_link_aliases_open_picker_and_escape_restores_mode() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('u'), KeyModifiers::empty()));
        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Open
        );

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(app.state.server_mode(), Mode::Navigate);
        assert!(app.state.work_link_picker.is_none());

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('U'), KeyModifiers::SHIFT));
        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Copy
        );
    }

    #[test]
    fn ac26_direct_legacy_alias_binding_dispatches_to_picker() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        app.state.keybinds.open_work_url = crate::config::ActionKeybinds::direct("u");

        assert!(app
            .handle_terminal_key_headless(TerminalKey::new(
                KeyCode::Char('u'),
                KeyModifiers::empty()
            ))
            .is_none());
        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Open
        );
    }

    #[test]
    fn ac26_open_link_picker_preserves_open_action() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);

        app.execute_tui_navigate_action(NavigateAction::OpenWorkLink, ActionContext::Prefix);

        assert_eq!(app.state.server_mode(), Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Open
        );
    }

    #[test]
    fn ac26_link_picker_revalidates_stale_snapshot_before_acting() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(Vec::new()),
                pr_urls: Some(Vec::new()),
                ..Default::default()
            })
            .unwrap();

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        assert!(app.event_rx.try_recv().is_err());
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("work link is stale")
        );
    }

    #[test]
    fn ac26_link_picker_revalidates_reordered_candidates_by_url() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();

        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-2".into(), "MAT-1".into()]),
                ..Default::default()
            })
            .unwrap();

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        match app.event_rx.try_recv().expect("picker clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://linear.app/scalable/issue/MAT-1")
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn ac26_single_link_is_single_shot_and_empty_context_is_a_noop() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                ..Default::default()
            })
            .unwrap();
        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert!(app.state.work_link_picker.is_none());
        assert!(matches!(
            app.event_rx
                .try_recv()
                .expect("single-shot clipboard event"),
            crate::events::AppEvent::ClipboardWrite { .. }
        ));

        let mut empty = app_with_test_workspaces(&["one"]);
        empty.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        assert!(empty.event_rx.try_recv().is_err());
        assert_eq!(empty.state.server_mode(), Mode::Terminal);
        assert_eq!(
            empty
                .state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("focused pane has no work link")
        );
    }

    #[test]
    fn next_agent_starts_at_first_visible_entry_when_focused_agent_is_filtered_out() {
        let mut app = app_with_test_workspaces(&["hidden", "first", "second"]);
        for ws_idx in 0..app.state.workspaces.len() {
            let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.set_raw_agent_state_for_test(if ws_idx == 0 {
                crate::detect::AgentState::Idle
            } else {
                crate::detect::AgentState::Working
            });
        }
        app.state.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(crate::api::schema::AgentViewFilter::Eq {
                field: crate::api::schema::AgentViewField::Builtin(
                    crate::api::schema::AgentViewBuiltinField::Status,
                ),
                value: crate::api::schema::AgentViewValue::String("working".to_string()),
            }),
            sort: Vec::new(),
        });

        app.execute_tui_navigate_action(NavigateAction::NextAgent, ActionContext::Prefix);

        assert_eq!(app.state.active, Some(1));
    }

    #[test]
    fn agent_picker_opens_remote_agents_on_this_client() {
        for action in [NavigateAction::NextAgent, NavigateAction::PreviousAgent] {
            let (mut app, agent_ref) = app_with_remote_agent();
            app.state.begin_workspace_picker_presentation();

            app.execute_tui_navigate_action(action, ActionContext::Prefix);

            assert_eq!(
                app.state.sidebar_selected_remote_agent,
                Some(agent_ref.clone())
            );
            assert_eq!(app.remote_focus_operations.len(), 1);
            assert!(app.state.toast.is_none());
        }
    }

    #[test]
    fn pane_cycle_opens_remote_agent_on_this_client_at_local_boundary() {
        for action in [
            NavigateAction::CyclePaneNext,
            NavigateAction::CyclePanePrevious,
        ] {
            let (mut app, agent_ref) = app_with_remote_agent();

            app.execute_tui_navigate_action(action, ActionContext::Prefix);

            assert_eq!(
                app.state.sidebar_selected_remote_agent,
                Some(agent_ref.clone())
            );
            assert_eq!(app.remote_focus_operations.len(), 1);
            assert!(app.state.toast.is_none());
        }
    }

    #[test]
    fn review_findings_agent_navigation_reveals_against_final_picker_projection() {
        let mut app = app_with_test_workspaces(&["one", "two", "three", "four", "five"]);
        for ws_idx in 0..app.state.workspaces.len() {
            let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.set_raw_agent_state_for_test(if ws_idx == 1 {
                crate::detect::AgentState::Idle
            } else {
                crate::detect::AgentState::Blocked
            });
        }
        app.state.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 30, 6);
        app.state.begin_workspace_picker_presentation();

        app.execute_tui_navigate_action(NavigateAction::NextAgent, ActionContext::Prefix);

        assert!(app.state.sidebar_shows_spaces_tree());
        let target_row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|row| {
                matches!(
                    row,
                    crate::ui::SidebarRow::Tab { entry, .. }
                        if entry.local_target().is_some_and(|target| {
                            target.ws_idx == 1 && target.tab_idx == 0
                        })
                )
            })
            .unwrap();
        let normalized = crate::ui::normalized_workspace_scroll(
            &app.state,
            app.state.view.sidebar_rect,
            app.state.workspace_scroll,
        );
        assert_eq!(
            app.state.workspace_scroll,
            crate::ui::sidebar_row_scroll_for_target(
                &app.state,
                app.state.view.sidebar_rect,
                normalized,
                target_row,
            )
        );
    }

    #[test]
    fn default_goto_key_opens_navigator() {
        let mut state = state_with_workspaces(&["test"]);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Navigator);
    }

    #[test]
    fn custom_rename_key_enters_rename_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.rename_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.effective_interaction_mode(), Mode::RenameWorkspace);
        assert_eq!(state.name_input, "test");
    }

    #[test]
    fn rename_workspace_prefills_live_terminal_cwd_label() {
        let mut state = state_with_workspaces(&["stale"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].panes[&root]
            .attached_terminal_id
            .clone();
        state.workspaces[0].custom_name = None;
        state.workspaces[0].identity_cwd = "/__herdr_original__".into();
        state.terminals.insert(
            terminal_id.clone(),
            TerminalState::new(terminal_id, "/__herdr_projects__".into()),
        );
        state.keybinds.rename_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.effective_interaction_mode(), Mode::RenameWorkspace);
        assert_eq!(state.name_input, "__herdr_projects__");
        assert_eq!(state.workspaces[0].display_name(), "__herdr_original__");
    }

    #[test]
    fn prefix_rename_workspace_targets_active_workspace_not_stale_selection() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.active = Some(1);
        state.selected = 0;
        state.set_server_mode(Mode::Prefix);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::RenameWorkspace,
            ActionContext::Prefix,
        );

        assert_eq!(state.effective_interaction_mode(), Mode::RenameWorkspace);
        assert_eq!(state.selected, 1);
        assert_eq!(state.name_input, "issue");
    }

    #[test]
    fn prefix_close_workspace_targets_active_linked_worktree_without_removing_checkout() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.active = Some(1);
        state.selected = 0;
        state.set_server_mode(Mode::Prefix);
        state.confirm_close = false;
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::CloseWorkspace,
            ActionContext::Prefix,
        );

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "main");
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn custom_new_workspace_key_requests_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.new_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.request_new_workspace);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn new_thread_key_opens_the_project_picker_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.new_thread = crate::config::ActionKeybinds::prefix("y");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()),
        );

        assert!(
            state.sidebar_new_thread.is_some(),
            "the shortcut asks which project before it spawns anything"
        );
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn new_workspace_key_opens_prefilled_prompt_and_preserves_captured_cwd() {
        let cwd = unique_temp_path("workspace-name-suggestion");
        std::fs::create_dir_all(&cwd).unwrap();
        let suggested_name = crate::workspace::derive_label_from_cwd(&cwd);
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.new_terminal_cwd =
            crate::config::NewTerminalCwdConfig::Path(cwd.display().to_string());
        app.state.prompt_new_workspace_name = true;
        app.state.set_server_mode(Mode::Navigate);
        app.state.keybinds.new_workspace = crate::config::ActionKeybinds::prefix("g");

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()));

        assert_eq!(
            app.state.effective_interaction_mode(),
            Mode::RenameWorkspace
        );
        assert_eq!(app.state.name_input, suggested_name);
        assert!(app.state.name_input_replace_on_type);
        assert_eq!(app.state.pending_workspace_create_cwd.as_ref(), Some(&cwd));
        assert_eq!(app.state.workspaces.len(), 1);

        app.state.new_terminal_cwd =
            crate::config::NewTerminalCwdConfig::Path("/tmp/changed-after-prompt".into());
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.workspaces[1].identity_cwd, cwd);
        assert!(app.state.workspaces[1].custom_name.is_none());
        assert!(app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(app.state.server_mode(), Mode::Navigate);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn api_rename_enter_keeps_auto_name_when_live_label_changes() {
        let mut app = app_with_test_workspaces(&["test"]);
        let tab = &app.state.workspaces[0].tabs[0];
        let terminal_id = tab
            .terminal_id(tab.layout.focused())
            .cloned()
            .expect("focused terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .agent_name = Some("claude".into());

        super::super::modal::open_rename_active_tab(&mut app.state, false);
        assert_eq!(app.state.name_input, "claude");

        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .agent_name = Some("codex".into());

        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(
            app.state.client_overlay,
            crate::app::state::ClientOverlay::None
        );
        assert!(
            app.state.workspaces[0].tabs[0].custom_name.is_none(),
            "an unedited Enter must not pin the stale prefill as a user name"
        );
    }

    #[tokio::test]
    async fn new_workspace_prompt_saves_custom_name_atomically() {
        let cwd = unique_temp_path("workspace-custom-name");
        std::fs::create_dir_all(&cwd).unwrap();
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.new_terminal_cwd =
            crate::config::NewTerminalCwdConfig::Path(cwd.display().to_string());
        app.state.prompt_new_workspace_name = true;
        app.state.set_server_mode(Mode::Navigate);

        app.execute_tui_navigate_action(NavigateAction::NewWorkspace, ActionContext::Navigate);
        app.state.name_input = "  logs  ".into();
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.workspaces[1].custom_name.as_deref(), Some("logs"));
        assert_eq!(app.state.workspaces[1].identity_cwd, cwd);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn cancelling_new_workspace_prompt_creates_nothing() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.prompt_new_workspace_name = true;
        app.state.set_server_mode(Mode::Navigate);

        app.execute_tui_navigate_action(NavigateAction::NewWorkspace, ActionContext::Navigate);
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 1);
        assert!(app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(
            app.state.client_overlay,
            crate::app::state::ClientOverlay::None
        );
    }

    #[test]
    fn custom_new_worktree_key_requests_selected_workspace() {
        let mut state = state_with_workspaces(&["main", "scratch"]);
        state.workspaces[1].identity_cwd = unique_temp_path("navigate-new-worktree-selected");
        state.set_server_mode(Mode::Navigate);
        state.selected = 1;
        state.active = Some(0);
        state.keybinds.new_worktree = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.request_new_linked_worktree, Some(1));
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn worktree_actions_do_not_start_from_linked_child_workspace() {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        let mut state = state_with_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 1, "repo-key");
        state.set_server_mode(Mode::Navigate);
        state.selected = 1;
        state.active = Some(0);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewWorktree,
            ActionContext::Navigate,
        );
        assert_eq!(state.request_new_linked_worktree, None);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::OpenWorktree,
            ActionContext::Navigate,
        );
        assert_eq!(state.request_open_existing_worktree, None);
    }

    #[test]
    fn direct_new_worktree_action_targets_active_workspace() {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        let mut state = state_with_workspaces(&["main", "scratch"]);
        state.workspaces[0].identity_cwd = unique_temp_path("navigate-new-worktree-active");
        state.set_server_mode(Mode::Terminal);
        state.selected = 1;
        state.active = Some(0);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewWorktree,
            ActionContext::Direct,
        );

        assert_eq!(state.request_new_linked_worktree, Some(0));
    }

    #[test]
    fn navigate_down_follows_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.set_server_mode(Mode::Navigate);
        state.active = Some(0);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 2);
    }

    #[test]
    fn navigate_number_keys_follow_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.set_server_mode(Mode::Navigate);
        state.active = Some(0);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty()),
        );

        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn indexed_switch_workspace_keybind_follows_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.set_server_mode(Mode::Prefix);
        state.active = Some(0);
        state.selected = 0;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::SwitchWorkspace(1),
            ActionContext::Prefix,
        );

        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn custom_sidebar_toggle_key_toggles_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.toggle_sidebar = crate::config::ActionKeybinds::prefix("g");
        assert!(!state.sidebar_collapsed);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.sidebar_collapsed);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn custom_resize_key_enters_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.resize_mode = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Resize);
    }

    #[test]
    fn custom_reload_config_key_requests_reload_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.reload_config = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.request_reload_config);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn custom_open_notification_key_focuses_current_toast_target() {
        let mut state = state_with_workspaces(&["one", "two"]);
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(Mode::Navigate);
        state.keybinds.open_notification_target = crate::config::ActionKeybinds::prefix("g");
        let target_workspace_id = state.workspaces[1].id.clone();
        let target_pane = state.workspaces[1].tabs[0].root_pane;
        state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".into(),
            context: "two".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id: target_workspace_id,
                pane_id: target_pane,
            }),
        });

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.active, Some(1));
        assert_eq!(state.selected, 1);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert!(state.toast.is_none());
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn movement_action_stays_in_navigate_mode() {
        let mut state = state_with_workspaces(&["a", "b"]);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn navigate_workspace_keys_are_configurable() {
        let mut state = state_with_workspaces(&["a", "b"]);
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn navigate_pane_keys_are_configurable() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn focus_pane_prefix_rhs_does_not_create_navigate_mode_pane_shortcut() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
focus_pane_down = "prefix+f"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn customized_navigate_pane_key_disables_matching_prefix_rhs_fallback() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn left_and_right_arrows_remain_permanent_navigate_pane_aliases() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(right);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_left = "ctrl+h"
navigate_pane_right = "ctrl+l"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
        assert_eq!(state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn mobile_workspace_keyboard_navigation_keeps_selected_row_visible() {
        let mut state = state_with_workspaces(&["a", "b", "c", "d"]);
        state.active = Some(0);
        state.selected = 0;
        state.set_server_mode(Mode::Navigate);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 44, 8));
        assert_eq!(state.mobile_switcher_scroll, 0);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.mobile_switcher_scroll, 0);
    }

    #[test]
    fn terminal_direct_agent_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.next_agent = crate::config::ActionKeybinds::direct("alt+a");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('a'), KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::NextAgent));
    }

    #[test]
    fn terminal_direct_focus_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.focus_pane_left = crate::config::ActionKeybinds::direct("alt+left");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Left, KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::FocusPaneLeft));
    }

    #[test]
    fn terminal_direct_swap_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.swap_pane_right = crate::config::ActionKeybinds::direct("alt+shift+l");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('l'), KeyModifiers::ALT | KeyModifiers::SHIFT),
        );

        assert_eq!(action, Some(NavigateAction::SwapPaneRight));
    }

    #[test]
    fn terminal_direct_resize_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.resize_pane_right =
            crate::config::ActionKeybinds::direct("ctrl+shift+alt+right");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(
                KeyCode::Right,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT,
            ),
        );

        assert_eq!(action, Some(NavigateAction::ResizePaneRight));
    }

    #[test]
    fn prefix_resize_pane_binding_maps_to_navigation_action() {
        let config: Config = toml::from_str(
            r#"
[keys]
resize_pane_left = "prefix+shift+left"
"#,
        )
        .unwrap();
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds = config.keybinds();

        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Left, KeyModifiers::SHIFT),
            BindingDispatch::Prefix,
        );

        assert_eq!(action, Some(NavigateAction::ResizePaneLeft));
    }

    #[test]
    fn terminal_direct_move_tab_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.move_tab_next = crate::config::ActionKeybinds::direct("alt+shift+right");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT),
        );

        assert_eq!(action, Some(NavigateAction::MoveTabNext));
    }

    fn tab_labels(state: &AppState) -> Vec<String> {
        let ws = &state.workspaces[0];
        (0..ws.tabs.len())
            .map(|tab_idx| ws.tab_display_name_from(&state.terminals, tab_idx).unwrap())
            .collect()
    }

    #[test]
    fn move_tab_actions_reorder_and_wrap_the_active_tab() {
        let mut state = state_with_workspaces(&["test"]);
        {
            let ws = &mut state.workspaces[0];
            ws.tabs[0].set_custom_name("a".into());
            ws.test_add_tab(Some("b"));
            ws.test_add_tab(Some("c"));
            ws.switch_tab(1);
        }

        execute_navigate_action(&mut state, NavigateAction::MoveTabNext);
        assert_eq!(tab_labels(&state), vec!["a", "c", "b"]);
        assert_eq!(state.workspaces[0].active_tab, 2);

        execute_navigate_action(&mut state, NavigateAction::MoveTabNext);
        assert_eq!(tab_labels(&state), vec!["b", "a", "c"]);
        assert_eq!(state.workspaces[0].active_tab, 0);

        execute_navigate_action(&mut state, NavigateAction::MoveTabPrevious);
        assert_eq!(tab_labels(&state), vec!["a", "c", "b"]);
        assert_eq!(state.workspaces[0].active_tab, 2);
        state.workspaces[0].assert_invariants_for_test();
    }

    #[test]
    fn move_tab_is_a_noop_with_a_single_tab() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].tabs[0].set_custom_name("only".into());

        execute_navigate_action(&mut state, NavigateAction::MoveTabNext);

        assert_eq!(tab_labels(&state), vec!["only"]);
        assert_eq!(state.workspaces[0].active_tab, 0);
    }

    #[test]
    fn move_tab_with_a_single_tab_still_exits_navigate_mode() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub,
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("solo")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);

        app.execute_tui_navigate_action(NavigateAction::MoveTabNext, ActionContext::Navigate);

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn terminal_direct_last_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.last_pane = crate::config::ActionKeybinds::direct("alt+l");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('l'), KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::LastPane));
    }

    #[test]
    fn generated_character_prefix_binding_falls_back_after_exact_chord() {
        let generated_key = crate::input::parse_terminal_key_sequence("\x1b[119;3;124u").unwrap();
        assert_eq!(generated_key.code, KeyCode::Char('w'));
        assert_eq!(generated_key.modifiers, KeyModifiers::ALT);
        assert_eq!(generated_key.generated_text.as_deref(), Some("|"));

        let generated_only: Config = toml::from_str(
            r#"
[keys]
split_vertical = "prefix+|"
"#,
        )
        .unwrap();
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds = generated_only.keybinds();
        assert!(matches!(
            prefix_binding_for_key(&state, &generated_key),
            Some(PrefixBindingMatch::Action(NavigateAction::SplitVertical))
        ));
        let multi_character_key =
            crate::input::parse_terminal_key_sequence("\x1b[119;3;124:120u").unwrap();
        assert!(prefix_binding_for_key(&state, &multi_character_key).is_none());

        let exact_and_generated: Config = toml::from_str(
            r#"
[keys]
split_vertical = "prefix+|"
split_horizontal = "prefix+alt+w"
"#,
        )
        .unwrap();
        state.keybinds = exact_and_generated.keybinds();
        assert!(matches!(
            prefix_binding_for_key(&state, &generated_key),
            Some(PrefixBindingMatch::Action(NavigateAction::SplitHorizontal))
        ));

        let exact_command: Config = toml::from_str(
            r#"
[keys]
split_vertical = "prefix+|"

[[keys.command]]
key = "prefix+alt+w"
command = "echo exact"
"#,
        )
        .unwrap();
        state.keybinds = exact_command.keybinds();
        assert!(matches!(
            prefix_binding_for_key(&state, &generated_key),
            Some(PrefixBindingMatch::Command(binding)) if binding.command == "echo exact"
        ));
    }

    #[test]
    fn shifted_backslash_layout_prefers_horizontal_split_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
split_vertical = "prefix+|"
split_horizontal = 'prefix+\'
"#,
        )
        .unwrap();
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds = config.keybinds();
        let key = crate::input::parse_terminal_key_sequence("\x1b[124:92;2:1u").unwrap();
        assert_eq!(key.code, KeyCode::Char('|'));
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
        assert_eq!(key.shifted_codepoint, Some('\\' as u32));
        assert!(state.keybinds.split_horizontal.matches_prefix_key(&key));
        assert!(!state.keybinds.split_vertical.matches_prefix_key(&key));

        assert_eq!(
            action_for_key(&state, key, BindingDispatch::Prefix),
            Some(NavigateAction::SplitHorizontal)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('|'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::SplitVertical)
        );
    }

    #[test]
    fn repository_editor_shortcut_maps_to_open_repo_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.editor_open_repo = crate::config::ActionKeybinds::direct("ctrl+alt+v");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
        );

        assert_eq!(action, Some(NavigateAction::OpenRepoEditor));
        execute_navigate_action(&mut state, action.expect("repository editor action"));
        assert!(state.request_open_repo_editor);
    }

    #[test]
    fn prefix_tab_override_can_map_to_last_pane() {
        let config: Config = toml::from_str(
            r#"
[keys]
last_pane = "prefix+tab"
"#,
        )
        .unwrap();
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds = config.keybinds();

        let pane_action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Tab, KeyModifiers::empty()),
            BindingDispatch::Prefix,
        );

        assert_eq!(pane_action, Some(NavigateAction::LastPane));
    }

    #[test]
    fn default_usage_keybinding_maps_to_the_usage_view() {
        let state = state_with_workspaces(&["test"]);
        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            BindingDispatch::Prefix,
        );
        assert_eq!(action, Some(NavigateAction::OpenUsageView));
    }

    #[test]
    fn terminal_direct_indexed_tab_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        let config: Config = toml::from_str("[keys]\nswitch_tab = \"ctrl+3\"\n").unwrap();
        state.keybinds.switch_tab = config.keybinds().switch_tab;

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('3'), KeyModifiers::CONTROL),
        );

        assert_eq!(action, Some(NavigateAction::SwitchTab(2)));
    }

    #[test]
    fn prefix_shift_indexed_workspace_shortcut_maps_legacy_us_symbol_key() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config =
            toml::from_str("[keys]\nswitch_workspace = \"prefix+shift+1..9\"\n").unwrap();
        state.keybinds.switch_workspace = config.keybinds().switch_workspace;

        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Char('@'), KeyModifiers::empty()),
            BindingDispatch::Prefix,
        );

        assert_eq!(action, Some(NavigateAction::SwitchWorkspace(1)));
    }

    #[test]
    fn prefix_shift_indexed_workspace_shortcut_maps_non_us_number_rows() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config =
            toml::from_str("[keys]\nswitch_workspace = \"prefix+shift+1..9\"\n").unwrap();
        state.keybinds.switch_workspace = config.keybinds().switch_workspace;

        for key in [
            TerminalKey::new(KeyCode::Char('2'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('"' as u32),
            TerminalKey::new(KeyCode::Char('é'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('2' as u32),
        ] {
            assert_eq!(
                action_for_key(&state, key, BindingDispatch::Prefix),
                Some(NavigateAction::SwitchWorkspace(1))
            );
        }
    }

    #[test]
    fn prefix_shift_indexed_workspace_shortcut_survives_modifier_press() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        let config: Config =
            toml::from_str("[keys]\nswitch_workspace = \"prefix+shift+1..9\"\n").unwrap();
        app.state.keybinds.switch_workspace = config.keybinds().switch_workspace;
        app.state.set_server_mode(Mode::Prefix);

        app.handle_prefix_key(TerminalKey::new(
            KeyCode::Modifier(ModifierKeyCode::LeftShift),
            KeyModifiers::SHIFT,
        ));

        assert_eq!(app.state.server_mode(), Mode::Prefix);

        app.handle_prefix_key(
            TerminalKey::new(KeyCode::Char('2'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('"' as u32),
        );

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn prefix_unshifted_indexed_shortcut_maps_shifted_french_number_row() {
        let mut state = state_with_workspaces(&["one"]);
        let config: Config = toml::from_str("[keys]\nswitch_tab = \"prefix+1..9\"\n").unwrap();
        state.keybinds.switch_tab = config.keybinds().switch_tab;

        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Char('é'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('2' as u32),
            BindingDispatch::Prefix,
        );

        assert_eq!(action, Some(NavigateAction::SwitchTab(1)));
    }

    #[test]
    fn literal_symbol_binding_takes_precedence_over_shifted_indexed_alias() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config = toml::from_str(
            r#"
[keys]
help = "prefix+!"
switch_workspace = "prefix+shift+1..9"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty()),
            BindingDispatch::Prefix,
        );

        assert_eq!(action, Some(NavigateAction::Help));
    }

    #[test]
    fn literal_symbol_custom_command_is_visible_before_shifted_indexed_alias() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config = toml::from_str(
            r#"
[keys]
switch_workspace = "prefix+shift+1..9"

[[keys.command]]
key = "prefix+!"
command = "echo literal"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        let key = TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty());
        assert!(command_for_key(&state, &key, BindingDispatch::Prefix).is_some());
        assert_eq!(
            indexed_navigation_action(&state, &key, BindingDispatch::Prefix),
            Some(NavigateAction::SwitchWorkspace(0))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn literal_symbol_custom_command_runs_before_shifted_indexed_alias() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.set_server_mode(Mode::Terminal);

        let output_path = unique_temp_path("literal-symbol-custom-command");
        let config: Config = toml::from_str(&format!(
            r#"
[keys]
switch_workspace = "prefix+shift+1..9"

[[keys.command]]
key = "prefix+!"
command = "printf literal > '{}'"
"#,
            output_path.display()
        ))
        .unwrap();
        app.state.keybinds = config.keybinds();

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty()))
            .await;

        assert_eq!(wait_for_file(&output_path), "literal");
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn navigate_mode_runs_prefix_action_rhs_without_pressing_prefix_again() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::SHIFT));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.server_mode(), Mode::Navigate);
        assert_eq!(app.state.effective_interaction_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn navigate_mode_matches_legacy_uppercase_shifted_letter() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.server_mode(), Mode::Navigate);
        assert_eq!(app.state.effective_interaction_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_workspace_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('W'), KeyModifiers::empty()));

        assert_eq!(
            app.state.effective_interaction_mode(),
            Mode::RenameWorkspace
        );
    }

    #[tokio::test]
    async fn kitty_shifted_alternate_without_modifier_prefers_reload_over_resize() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Prefix);

        let mut events = parse_raw_input_bytes_sync(b"\x1b[114:82;1u");
        assert_eq!(events.len(), 1);
        let RawInputEvent::Key(key) = events.remove(0) else {
            panic!("expected key event");
        };
        assert_eq!(
            action_for_key(&app.state, key.clone(), BindingDispatch::Prefix),
            Some(NavigateAction::ReloadConfig)
        );
        app.handle_prefix_key(key);

        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_reload_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('R'), KeyModifiers::empty()));

        assert!(!app.state.request_reload_config);
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_pane_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('P'), KeyModifiers::empty()));

        assert_eq!(app.state.effective_interaction_mode(), Mode::RenamePane);
    }

    #[test]
    fn app_navigate_mode_workspace_down_moves_selection() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Down, KeyModifiers::empty()));

        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn app_navigate_mode_maps_french_number_row_to_workspace() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(
            TerminalKey::new(KeyCode::Char('é'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('2' as u32),
        );

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn app_navigate_mode_workspace_keys_are_configurable() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        app.state.keybinds = config.keybinds();
        app.state.set_server_mode(Mode::Navigate);

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()));

        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.server_mode(), Mode::Navigate);
    }

    #[tokio::test]
    async fn prefix_focus_pane_is_one_shot_and_returns_to_terminal() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.workspaces[0].layout.focus_pane(right);
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn navigate_focus_pane_keeps_navigate_mode_active() {
        let mut app = app_with_test_workspaces(&["test"]);
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let below = app.state.workspaces[0].test_split(Direction::Vertical);
        app.state.workspaces[0].layout.focus_pane(below);
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        app.state.set_server_mode(Mode::Navigate);

        app.handle_key(TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(app.state.server_mode(), Mode::Navigate);
    }

    #[tokio::test]
    async fn no_op_prefix_action_exits_prefix_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('o'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn unmatched_prefix_rhs_exits_prefix_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::F(12), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[tokio::test]
    async fn prefix_help_matches_enhanced_shifted_question_mark() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        )
        .await;

        assert_eq!(app.state.server_mode(), Mode::KeybindHelp);
    }

    #[test]
    fn navigate_mode_help_is_binding_driven() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.help = crate::config::ActionKeybinds::prefix("f");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.server_mode(), Mode::Navigate);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::empty()),
        );
        assert_eq!(state.server_mode(), Mode::KeybindHelp);
    }

    #[test]
    fn modified_navigate_local_key_can_be_bound_as_prefix_rhs() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.toggle_sidebar = crate::config::ActionKeybinds::prefix("shift+u");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT),
        );

        assert!(state.sidebar_collapsed);
    }

    #[test]
    fn empty_state_new_tab_is_no_op() {
        let mut state = crate::app::state::AppState::test_new();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Prefix);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewTab,
            ActionContext::Prefix,
        );

        assert_eq!(state.server_mode(), Mode::Navigate);
        assert!(!state.creating_new_tab);
        assert!(!state.request_new_tab);
        assert!(state.workspaces.is_empty());
    }

    #[test]
    fn closing_linked_worktree_closes_workspace_without_removing_checkout() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        state.selected = 1;
        state.active = Some(1);
        state.set_server_mode(Mode::Navigate);
        state.confirm_close = false;
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        execute_navigate_action(&mut state, NavigateAction::CloseWorkspace);

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "main");
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn tui_close_parent_group_closes_immediately_when_confirmation_disabled() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);
        app.state.confirm_close = false;

        app.execute_tui_navigate_action(NavigateAction::CloseWorkspace, ActionContext::Navigate);

        assert!(app.state.workspaces.is_empty());
        assert_eq!(app.state.server_mode(), Mode::Navigate);
        assert_eq!(app.event_hub.events_after(0).len(), 2);
    }

    #[test]
    fn prefix_close_pane_last_parent_group_pane_confirms_group_close() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 1, "repo-key");
        state.selected = 1;
        state.active = Some(0);
        state.confirm_close = true;
        state.set_server_mode(Mode::Navigate);

        execute_navigate_action(&mut state, NavigateAction::ClosePane);

        assert_eq!(state.selected, 0);
        assert_eq!(state.effective_interaction_mode(), Mode::ConfirmClose);
        assert_eq!(state.workspaces.len(), 2);
    }

    #[tokio::test]
    async fn tui_close_tab_last_tab_closes_workspace() {
        let mut app = app_with_test_workspaces(&["main"]);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);
        app.state.confirm_close = false;

        app.execute_tui_navigate_action(NavigateAction::CloseTab, ActionContext::Navigate);

        assert!(app.state.workspaces.is_empty());
        assert_eq!(app.state.active, None);
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[tokio::test]
    async fn tui_close_last_pane_closes_workspace() {
        let mut app = app_with_test_workspaces(&["main"]);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);
        app.state.confirm_close = false;

        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Navigate);

        assert!(app.state.workspaces.is_empty());
        assert_eq!(app.state.active, None);
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[tokio::test]
    async fn tui_close_tab_last_parent_group_asks_for_confirmation() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.confirm_close = true;
        app.state.set_server_mode(Mode::Navigate);

        app.execute_tui_navigate_action(NavigateAction::CloseTab, ActionContext::Navigate);

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.effective_interaction_mode(), Mode::ConfirmClose);
        assert!(app.event_hub.events_after(0).is_empty());
    }

    #[tokio::test]
    async fn tui_close_pane_last_parent_group_pane_confirms_group_close() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.confirm_close = true;
        app.state.set_server_mode(Mode::Navigate);

        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Navigate);

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.selected, 0);
        assert!(app.state.workspaces[0].pane_state(pane_id).is_some());
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.effective_interaction_mode(), Mode::ConfirmClose);
        assert!(!app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn custom_command_runs_from_prefix_key_in_navigate_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let output_path = unique_temp_path("custom-command-keybind");
        let release_path = unique_temp_path("custom-command-release");
        let command = format!(
            "printf '%s\\n%s\\n%s\\n%s\\n' \"$$\" \"$HERDR_ACTIVE_WORKSPACE_ID\" \"$HERDR_ACTIVE_TAB_ID\" \"$HERDR_ACTIVE_PANE_ID\" > '{}'; i=0; while [ ! -e '{}' ] && [ \"$i\" -lt 250 ]; do sleep 0.02; i=$((i + 1)); done",
            output_path.display(),
            release_path.display(),
        );
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("m"),
            label: "prefix+m".into(),
            command,
            action: crate::config::CustomCommandAction::Shell,
            description: None,
            width: None,
            height: None,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        assert_eq!(app.state.server_mode(), Mode::Prefix);

        let launch_started = std::time::Instant::now();
        app.handle_key(TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
            .await;
        assert!(launch_started.elapsed() < Duration::from_secs(2));

        let content = wait_for_file(&output_path);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4);
        let pid = lines[0]
            .parse::<u32>()
            .expect("command should report its pid");
        assert!(crate::platform::process_exists(pid));
        assert_eq!(lines[1], app.state.workspaces[0].id);
        assert_eq!(lines[2], format!("{}:t1", app.state.workspaces[0].id));
        assert_eq!(lines[3], format!("{}:p1", app.state.workspaces[0].id));
        assert_eq!(app.state.server_mode(), Mode::Terminal);

        std::fs::write(&release_path, b"release").expect("release command");
        let reaped_by_runtime = wait_for_custom_command_reap(&mut app, pid).await;
        if !reaped_by_runtime {
            if let Some(child) = app
                .detached_custom_command_children
                .iter_mut()
                .find(|child| child.id() == pid)
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        assert!(
            reaped_by_runtime,
            "detached command child {pid} was not reaped"
        );

        let _ = std::fs::remove_file(output_path);
        let _ = std::fs::remove_file(release_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pane_overlay_command_opens_and_closes_after_exit() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (workspace, terminal, runtime) = Workspace::new(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
            24,
            80,
            app.state.pane_scrollback_limit_bytes,
            app.state.host_terminal_theme,
            app.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&app.state.default_shell, app.state.shell_mode),
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("workspace should spawn");
        let root_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.terminal_runtimes.insert(terminal.id.clone(), runtime);
        app.state.terminals.insert(terminal.id.clone(), terminal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let output_path = unique_temp_path("custom-pane-command");
        let command = format!("printf done > '{}'", output_path.display());
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("m"),
            label: "prefix+m".into(),
            command,
            action: crate::config::CustomCommandAction::Pane,
            description: None,
            width: None,
            height: None,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(app.terminal_runtimes.len(), 2);
        assert!(app.state.workspaces[0].tabs[0].zoomed);
        let overlay_pane = app.state.workspaces[0].focused_pane_id().unwrap();
        assert_ne!(overlay_pane, root_pane);

        app.state.last_pane();

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root_pane));

        app.state.last_pane();

        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(overlay_pane)
        );

        let _ = wait_for_file(&output_path);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if app.drain_internal_events()
                && app.state.workspaces[0].tabs[0].layout.pane_count() == 1
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert!(!app.state.workspaces[0].tabs[0].zoomed);
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        let _ = std::fs::remove_file(output_path);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_scrollback_key_preserves_logical_lines_in_editor_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                5,
                5,
                4096,
                b"ABCDEFGHIJ\r\nKLMNO",
            ),
        );
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);

        let output_path = unique_temp_path("edit-scrollback");
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(
            "EDITOR",
            format!("sh -c 'cp \"$1\" {}' sh", output_path.display()),
        );
        app.state.keybinds.edit_scrollback = crate::config::ActionKeybinds::prefix("g");

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()))
            .await;

        drop(env);

        let content = wait_for_file(&output_path);
        assert_eq!(content, "ABCDEFGHIJ\nKLMNO");
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert!(
            app.state.terminals.values().any(|terminal| terminal
                .launch_argv
                .as_ref()
                .is_some_and(|argv| argv.first().is_some_and(|program| program == "/bin/sh"))),
            "scrollback editor should launch through argv overlay path"
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn zoom_action_exits_navigate_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        state.keybinds.zoom = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn focus_pane_action_keeps_zoomed_when_changing_focus() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        execute_navigate_action(&mut state, NavigateAction::FocusPaneRight);

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn question_mark_opens_keybind_help_from_navigate() {
        let mut state = state_with_workspaces(&["test"]);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );

        assert_eq!(state.server_mode(), Mode::KeybindHelp);
    }

    #[test]
    fn new_tab_action_opens_dialog_without_creating_tab() {
        let mut state = state_with_workspaces(&["test"]);

        execute_navigate_action(&mut state, NavigateAction::NewTab);

        assert_eq!(state.effective_interaction_mode(), Mode::RenameTab);
        assert!(state.creating_new_tab);
        assert_eq!(state.name_input, "2");
        assert!(state.name_input_replace_on_type);
        assert!(!state.request_new_tab);
        assert_eq!(state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn new_tab_action_can_skip_rename_dialog() {
        let mut state = state_with_workspaces(&["test"]);
        state.prompt_new_tab_name = false;

        execute_navigate_action(&mut state, NavigateAction::NewTab);

        assert_eq!(state.server_mode(), Mode::Terminal);
        assert!(!state.creating_new_tab);
        assert!(state.request_new_tab);
        assert!(state.requested_new_tab_name.is_none());
    }

    #[test]
    fn navigate_q_detaches_in_persistence_mode() {
        let mut state = crate::app::state::AppState::test_new();
        state.detach_exits = false;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
        );

        assert!(state.detach_requested);
        assert!(!state.should_quit);
    }

    #[test]
    fn user_action_key_dispatch_respects_trigger_and_repo_scope() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.user_actions = vec![crate::config::UserAction {
            name: "test".into(),
            command: "just test".into(),
            bindings: crate::config::ActionKeybinds::prefix("t"),
            run_on_worktree_create: false,
            open_in_bottom_pane: true,
            repo: None,
        }];
        let key = TerminalKey::new(KeyCode::Char('t'), KeyModifiers::empty());

        assert_eq!(
            user_action_for_key(&state, &key, BindingDispatch::Prefix),
            Some(0)
        );
        assert_eq!(
            user_action_for_key(&state, &key, BindingDispatch::Direct),
            None
        );
        state.keybinds.user_actions[0].repo = Some("other/repo".into());
        assert_eq!(
            user_action_for_key(&state, &key, BindingDispatch::Prefix),
            None
        );
    }
}
