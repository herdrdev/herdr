use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{
    input::{ActionContext, NavigateAction},
    settings_keybindings::built_in_keybinding_entries,
    state::{AppState, CommandPaletteState, Mode},
    App,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteCommand {
    BuiltIn(NavigateAction),
    ToggleNotifications,
    UserAction(usize),
    CustomCommand(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaletteEntry {
    pub group: String,
    pub label: String,
    pub key: String,
    pub command: PaletteCommand,
    score: i32,
}

pub(crate) fn action_for_field(field: &str) -> Option<NavigateAction> {
    Some(match field {
        "help" => NavigateAction::Help,
        "settings" => NavigateAction::Settings,
        "command_palette" => NavigateAction::OpenCommandPalette,
        "detach" => NavigateAction::Detach,
        "reload_config" => NavigateAction::ReloadConfig,
        "open_notification_target" => NavigateAction::OpenNotificationTarget,
        // The legacy URL aliases precede the canonical fields in the real
        // dispatch table, so these defaults resolve to the alias variants.
        "open_work_link" => NavigateAction::OpenWorkUrl,
        "copy_work_link" => NavigateAction::CopyWorkUrl,
        "copy_work_ticket" => NavigateAction::CopyWorkTicket,
        "copy_work_pr" => NavigateAction::CopyWorkPr,
        "copy_work_preview" => NavigateAction::CopyWorkPreview,
        "toggle_theme" => NavigateAction::ToggleTheme,
        "workspace_picker" => NavigateAction::WorkspacePicker,
        "goto" => NavigateAction::OpenNavigator,
        "new_workspace" => NavigateAction::NewWorkspace,
        "new_thread" => NavigateAction::NewThread,
        "new_agent_dock" => NavigateAction::NewAgentDock,
        "agent_finder" => NavigateAction::OpenAgentFinder,
        "new_worktree" => NavigateAction::NewWorktree,
        "open_worktree" => NavigateAction::OpenWorktree,
        "remove_worktree" => NavigateAction::RemoveWorktree,
        "rename_workspace" => NavigateAction::RenameWorkspace,
        "close_workspace" => NavigateAction::CloseWorkspace,
        "previous_workspace" => NavigateAction::PreviousWorkspace,
        "next_workspace" => NavigateAction::NextWorkspace,
        "new_tab" => NavigateAction::NewTab,
        "rename_tab" => NavigateAction::RenameTab,
        "close_tab" => NavigateAction::CloseTab,
        "toggle_tab_prio" => NavigateAction::ToggleTabPrio,
        "toggle_pin_tab" => NavigateAction::TogglePinTab,
        "previous_tab" => NavigateAction::PreviousTab,
        "next_tab" => NavigateAction::NextTab,
        "move_tab_previous" => NavigateAction::MoveTabPrevious,
        "move_tab_next" => NavigateAction::MoveTabNext,
        "previous_window" => NavigateAction::PreviousWindow,
        "next_window" => NavigateAction::NextWindow,
        "next_blocked_window" => NavigateAction::NextBlockedWindow,
        "focus_pane_left" => NavigateAction::FocusPaneLeft,
        "focus_pane_down" => NavigateAction::FocusPaneDown,
        "focus_pane_up" => NavigateAction::FocusPaneUp,
        "focus_pane_right" => NavigateAction::FocusPaneRight,
        "swap_pane_left" => NavigateAction::SwapPaneLeft,
        "swap_pane_down" => NavigateAction::SwapPaneDown,
        "swap_pane_up" => NavigateAction::SwapPaneUp,
        "swap_pane_right" => NavigateAction::SwapPaneRight,
        "cycle_pane_next" => NavigateAction::CyclePaneNext,
        "cycle_pane_previous" => NavigateAction::CyclePanePrevious,
        "last_pane" => NavigateAction::LastPane,
        "split_vertical" => NavigateAction::SplitVertical,
        "split_horizontal" => NavigateAction::SplitHorizontal,
        "split_left" => NavigateAction::SplitLeft,
        "split_up" => NavigateAction::SplitUp,
        "close_pane" => NavigateAction::ClosePane,
        "zoom" => NavigateAction::Zoom,
        "resize_mode" => NavigateAction::EnterResizeMode,
        "resize_pane_left" => NavigateAction::ResizePaneLeft,
        "resize_pane_down" => NavigateAction::ResizePaneDown,
        "resize_pane_up" => NavigateAction::ResizePaneUp,
        "resize_pane_right" => NavigateAction::ResizePaneRight,
        "rename_pane" => NavigateAction::RenamePane,
        "edit_scrollback" => NavigateAction::EditScrollback,
        "copy_mode" => NavigateAction::CopyMode,
        "toggle_sidebar" => NavigateAction::ToggleSidebar,
        "focus_sidebar" => NavigateAction::FocusSidebar,
        "assign_pane_to_pod" => NavigateAction::AssignPaneToPod,
        "focus_owning_repo_group" => NavigateAction::FocusOwningRepoGroup,
        "sidebar_cycle_group_mode" => NavigateAction::CycleSidebarGroupMode,
        "sidebar_refresh" => NavigateAction::RefreshSidebar,
        "toggle_blocked_filter" => NavigateAction::ToggleBlockedFilter,
        "toggle_prio_panel" => NavigateAction::TogglePrioPanel,
        "toggle_dock" => NavigateAction::ToggleDock,
        "previous_dock_tab" => NavigateAction::PreviousDockTab,
        "next_dock_tab" => NavigateAction::NextDockTab,
        "editor_open_repo" => NavigateAction::OpenRepoEditor,
        "toggle_status_detail" => NavigateAction::ToggleStatusDetail,
        "edit_scratchpad" => NavigateAction::EditScratchpad,
        "show_scratchpad" => NavigateAction::ShowScratchpad,
        "toggle_notepad" => NavigateAction::ToggleNotepad,
        "toggle_pomodoro" => NavigateAction::TogglePomodoro,
        "home" => NavigateAction::OpenHome,
        "work" => NavigateAction::OpenWorkView,
        "usage" => NavigateAction::OpenUsageView,
        "tickets" => NavigateAction::OpenTicketView,
        "inbox" => NavigateAction::OpenInbox,
        "missive" => NavigateAction::OpenMissiveView,
        "symphony" => NavigateAction::OpenSymphony,
        "runs" => NavigateAction::OpenRuns,
        "git_pull" => NavigateAction::GitPull,
        "git_commit" => NavigateAction::GitCommit,
        "git_push" => NavigateAction::GitPush,
        "git_create_pr" => NavigateAction::GitCreatePr,
        "dock_home" => NavigateAction::OpenDockHome,
        "dock_terminal" => NavigateAction::OpenDockTerminal,
        "dock_files" => NavigateAction::OpenDockFiles,
        "dock_diff" => NavigateAction::OpenDockDiff,
        "dock_pr" => NavigateAction::OpenDockPr,
        "dock_linear" => NavigateAction::OpenDockLinear,
        "dock_missive" => NavigateAction::OpenDockMissive,
        "dock_agents" => NavigateAction::OpenDockAgents,
        "dock_shortcuts" => NavigateAction::OpenDockShortcuts,
        "dock_context" => NavigateAction::OpenDockContext,
        "dock_symphony" => NavigateAction::OpenDockSymphony,
        "next_review_agent" => NavigateAction::NextReviewAgent,
        "previous_agent" => NavigateAction::PreviousAgent,
        "next_agent" => NavigateAction::NextAgent,
        "navigate_workspace_up"
        | "navigate_workspace_down"
        | "navigate_pane_left"
        | "navigate_pane_down"
        | "navigate_pane_up"
        | "navigate_pane_right" => return None,
        _ => return None,
    })
}

pub(crate) fn fuzzy_score(text: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let text_chars: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let query_chars: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if query_chars.is_empty() {
        return Some(0);
    }

    let mut score = 0;
    let mut text_index = 0usize;
    let mut previous_match = None;
    for query_char in query_chars {
        let relative_index = text_chars[text_index..]
            .iter()
            .position(|candidate| *candidate == query_char)?;
        let match_index = text_index + relative_index;
        let gap = previous_match.map_or(match_index, |previous| {
            match_index.saturating_sub(previous + 1)
        });
        score += 1;
        score -= gap.min(4) as i32;
        if previous_match == Some(match_index.saturating_sub(1)) {
            score += 8;
        }
        if match_index == 0
            || matches!(
                text_chars.get(match_index.saturating_sub(1)),
                Some(' ' | '_' | '-' | '.' | '/')
            )
        {
            score += 6;
        }
        previous_match = Some(match_index);
        text_index = match_index + 1;
    }

    Some(score - (text_chars.len() / 32) as i32)
}

fn score_entry(group: &str, label: &str, query: &str) -> Option<i32> {
    let label_score = fuzzy_score(label, query);
    let combined = format!("{group} {label}");
    let combined_score = fuzzy_score(&combined, query);
    match (label_score, combined_score) {
        (None, None) => None,
        (Some(score), None) => Some(score),
        (None, Some(score)) => Some(score - 4),
        (Some(label_score), Some(combined_score)) => Some(label_score.max(combined_score)),
    }
}

impl AppState {
    pub(crate) fn command_palette_entries(&self) -> Vec<PaletteEntry> {
        let query = self.command_palette.query.as_str();
        let mut entries = Vec::new();

        for (group, field, label, accessor) in built_in_keybinding_entries() {
            let Some(action) = action_for_field(field) else {
                continue;
            };
            let Some(score) = score_entry(group, label, query) else {
                continue;
            };
            entries.push(PaletteEntry {
                group: group.to_string(),
                label: label.to_string(),
                key: accessor(&self.keybinds).label().unwrap_or_default(),
                command: PaletteCommand::BuiltIn(action),
                score,
            });
        }

        let group = "pane";
        let label = "assign pane to pod";
        if let Some(score) = score_entry(group, label, query) {
            entries.push(PaletteEntry {
                group: group.to_string(),
                label: label.to_string(),
                key: String::new(),
                command: PaletteCommand::BuiltIn(NavigateAction::AssignPaneToPod),
                score,
            });
        }

        let group = "global";
        let label = "toggle notifications";
        if let Some(score) = score_entry(group, label, query) {
            entries.push(PaletteEntry {
                group: group.to_string(),
                label: label.to_string(),
                key: String::new(),
                command: PaletteCommand::ToggleNotifications,
                score,
            });
        }

        let repo = self.focused_repo_slug();
        for (index, action) in self.keybinds.user_actions.iter().enumerate() {
            if !action.applies_to_repo(repo.as_deref()) {
                continue;
            }
            let group = "actions";
            let Some(score) = score_entry(group, &action.name, query) else {
                continue;
            };
            entries.push(PaletteEntry {
                group: group.to_string(),
                label: action.name.clone(),
                key: action.bindings.label().unwrap_or_default(),
                command: PaletteCommand::UserAction(index),
                score,
            });
        }

        for (index, command) in self.keybinds.custom_commands.iter().enumerate() {
            let group = "commands";
            let label = command
                .description
                .as_deref()
                .filter(|description| !description.trim().is_empty())
                .unwrap_or(&command.command);
            let Some(score) = score_entry(group, label, query) else {
                continue;
            };
            entries.push(PaletteEntry {
                group: group.to_string(),
                label: label.to_string(),
                key: command.bindings.label().unwrap_or_default(),
                command: PaletteCommand::CustomCommand(index),
                score,
            });
        }

        if !query.is_empty() {
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.score));
        }
        entries
    }

    pub(crate) fn open_command_palette(&mut self) {
        self.command_palette = CommandPaletteState::default();
        self.set_server_mode(Mode::CommandPalette);
    }

    pub(crate) fn command_palette_visible_rows(&self) -> usize {
        self.command_palette_body_rect()
            .map_or(0, |body| body.height as usize)
    }

    pub(crate) fn command_palette_max_scroll(&self) -> usize {
        self.command_palette_entries()
            .len()
            .saturating_sub(self.command_palette_visible_rows())
    }

    pub(crate) fn clamp_command_palette_selection(&mut self) {
        let entry_count = self.command_palette_entries().len();
        if entry_count == 0 {
            self.command_palette.selected = 0;
            self.command_palette.scroll = 0;
            return;
        }
        self.command_palette.selected = self.command_palette.selected.min(entry_count - 1);
        self.command_palette.scroll = self
            .command_palette
            .scroll
            .min(self.command_palette_max_scroll());
    }

    pub(crate) fn ensure_command_palette_selection_visible(&mut self) {
        self.clamp_command_palette_selection();
        let visible_rows = self.command_palette_visible_rows();
        if visible_rows == 0 {
            self.command_palette.scroll = 0;
            return;
        }
        if self.command_palette.selected < self.command_palette.scroll {
            self.command_palette.scroll = self.command_palette.selected;
        } else if self.command_palette.selected >= self.command_palette.scroll + visible_rows {
            self.command_palette.scroll = self
                .command_palette
                .selected
                .saturating_add(1)
                .saturating_sub(visible_rows);
        }
        self.command_palette.scroll = self
            .command_palette
            .scroll
            .min(self.command_palette_max_scroll());
    }

    pub(crate) fn move_command_palette_selection(&mut self, delta: isize) {
        let entry_count = self.command_palette_entries().len();
        if entry_count == 0 {
            self.command_palette.selected = 0;
            self.command_palette.scroll = 0;
            return;
        }
        let current = self.command_palette.selected.min(entry_count - 1);
        let offset = delta.unsigned_abs() % entry_count;
        self.command_palette.selected = if delta.is_negative() {
            (current + entry_count - offset) % entry_count
        } else {
            (current + offset) % entry_count
        };
        self.ensure_command_palette_selection_visible();
    }

    pub(crate) fn insert_command_palette_query_text(&mut self, text: &str) {
        self.command_palette
            .query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.command_palette.selected = 0;
        self.command_palette.scroll = 0;
        self.clamp_command_palette_selection();
    }

    pub(crate) fn selected_command_palette_command(&self) -> Option<PaletteCommand> {
        self.command_palette_entries()
            .get(self.command_palette.selected)
            .map(|entry| entry.command)
    }
}

impl App {
    pub(crate) fn handle_command_palette_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state.command_palette = CommandPaletteState::default();
                self.state.set_server_mode(Mode::Terminal);
            }
            KeyCode::Enter => self.accept_command_palette_selection(),
            KeyCode::Backspace => {
                self.state.command_palette.query.pop();
                self.state.command_palette.selected = 0;
                self.state.command_palette.scroll = 0;
                self.state.clamp_command_palette_selection();
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                self.state.move_command_palette_selection(-1)
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.state.move_command_palette_selection(1)
            }
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_command_palette_selection(-1)
            }
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.move_command_palette_selection(1)
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.state.command_palette.query.clear();
                self.state.command_palette.selected = 0;
                self.state.command_palette.scroll = 0;
            }
            KeyCode::PageUp => {
                let page = self.state.command_palette_visible_rows().max(1) as isize;
                self.state.move_command_palette_selection(-page);
            }
            KeyCode::PageDown => {
                let page = self.state.command_palette_visible_rows().max(1) as isize;
                self.state.move_command_palette_selection(page);
            }
            KeyCode::Home => {
                self.state.command_palette.selected = 0;
                self.state.command_palette.scroll = 0;
            }
            KeyCode::End => {
                self.state.command_palette.selected =
                    self.state.command_palette_entries().len().saturating_sub(1);
                self.state.ensure_command_palette_selection_visible();
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.state
                    .insert_command_palette_query_text(&character.to_string());
            }
            _ => {}
        }
    }

    pub(crate) fn accept_command_palette_selection(&mut self) {
        let Some(command) = self.state.selected_command_palette_command() else {
            return;
        };

        self.state.set_server_mode(Mode::Terminal);
        self.state.command_palette = CommandPaletteState::default();

        match command {
            PaletteCommand::BuiltIn(action) => {
                self.execute_tui_navigate_action(action, ActionContext::Prefix);
            }
            PaletteCommand::ToggleNotifications => self.toggle_notifications(),
            PaletteCommand::UserAction(index) => {
                if self.state.keybinds.user_actions.get(index).is_some() {
                    self.state.request_user_action = Some(index);
                }
            }
            PaletteCommand::CustomCommand(index) => {
                if let Some(binding) = self.state.keybinds.custom_commands.get(index).cloned() {
                    self.launch_custom_command(binding, ActionContext::Prefix);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::input::{
            action_for_key_for_test, non_indexed_navigation_actions_for_test, BindingDispatch,
        },
        config::ActionKeybinds,
        input::TerminalKey,
    };

    #[test]
    fn fuzzy_score_requires_an_ordered_subsequence() {
        assert!(fuzzy_score("split vertical", "sv").is_some());
        assert!(fuzzy_score("split vertical", "vs").is_none());
        assert_eq!(fuzzy_score("anything", ""), Some(0));
    }

    #[test]
    fn word_start_matches_outrank_mid_word_matches() {
        assert!(fuzzy_score("split", "s") > fuzzy_score("isplit", "s"));
    }

    #[test]
    fn split_vertical_ranks_first_for_a_specific_query() {
        let mut state = AppState::test_new();
        state.command_palette.query = "split vert".to_string();
        let entries = state.command_palette_entries();
        assert_eq!(
            entries.first().map(|entry| entry.label.as_str()),
            Some("split vertical")
        );
    }

    #[test]
    fn empty_query_keeps_built_in_table_order_and_is_non_trivial() {
        let state = AppState::test_new();
        let entries = state.command_palette_entries();
        assert_eq!(
            entries.first().map(|entry| entry.label.as_str()),
            Some("keybinds")
        );
        assert!(entries.len() > 40);
    }

    #[test]
    fn new_palette_commands_have_their_expected_labels_and_actions() {
        let state = AppState::test_new();
        let entries = state.command_palette_entries();
        let expected = [
            ("missive", NavigateAction::OpenMissiveView),
            ("move tab previous", NavigateAction::MoveTabPrevious),
            ("move tab next", NavigateAction::MoveTabNext),
            ("resize pane left", NavigateAction::ResizePaneLeft),
            ("resize pane down", NavigateAction::ResizePaneDown),
            ("resize pane up", NavigateAction::ResizePaneUp),
            ("resize pane right", NavigateAction::ResizePaneRight),
            ("git pull", NavigateAction::GitPull),
            ("git commit", NavigateAction::GitCommit),
            ("git push", NavigateAction::GitPush),
            ("git create pull request", NavigateAction::GitCreatePr),
            ("dock: home", NavigateAction::OpenDockHome),
            ("dock: terminal", NavigateAction::OpenDockTerminal),
            ("dock: files", NavigateAction::OpenDockFiles),
            ("dock: diff", NavigateAction::OpenDockDiff),
            ("dock: pull requests", NavigateAction::OpenDockPr),
            ("dock: linear", NavigateAction::OpenDockLinear),
            ("dock: missive", NavigateAction::OpenDockMissive),
            ("dock: agents", NavigateAction::OpenDockAgents),
            ("dock: shortcuts", NavigateAction::OpenDockShortcuts),
            ("dock: context", NavigateAction::OpenDockContext),
            ("dock: symphony", NavigateAction::OpenDockSymphony),
            ("toggle dark/light theme", NavigateAction::ToggleTheme),
            ("focus sidebar", NavigateAction::FocusSidebar),
            ("assign pane to pod", NavigateAction::AssignPaneToPod),
        ];

        for (label, action) in expected {
            let entry = entries
                .iter()
                .find(|entry| entry.label == label)
                .unwrap_or_else(|| panic!("missing palette command {label:?}"));
            assert_eq!(entry.command, PaletteCommand::BuiltIn(action));
        }
        assert!(entries.iter().any(|entry| entry.label == "missive"));
    }

    #[test]
    fn fuzzy_queries_find_the_new_palette_commands() {
        let mut state = AppState::test_new();
        for (query, label) in [
            ("diff", "dock: diff"),
            ("push", "git push"),
            ("theme", "toggle dark/light theme"),
        ] {
            state.command_palette.query = query.to_string();
            assert_eq!(
                state
                    .command_palette_entries()
                    .first()
                    .map(|entry| entry.label.as_str()),
                Some(label),
                "query {query:?}"
            );
        }
    }

    #[test]
    fn command_palette_toggles_notifications_for_this_machine() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-notification-palette-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("temp config directory");
        let path = directory.join("config.toml");
        std::fs::write(&path, "# per-machine config\n").expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.local_sound_playback = false;
        app.state.open_command_palette();
        app.state.command_palette.query = "toggle notifications".to_string();
        assert_eq!(
            app.state.selected_command_palette_command(),
            Some(PaletteCommand::ToggleNotifications)
        );

        app.accept_command_palette_selection();

        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Terminal
        );
        assert!(app.state.sound.enabled);
        assert_eq!(app.state.request_client_notification_config, Some(true));
        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(&directory).ok();
    }

    /// The dispatch pair list and the settings/palette table are independent
    /// hand-maintained surfaces. This catches a keybindable action that is
    /// dispatched but missing from `BUILT_IN_GROUPS`. The help table is a
    /// third independently maintained table and has already drifted from the
    /// settings table in both directions, so it is documented here rather
    /// than forced into a larger cleanup.
    #[test]
    fn every_non_indexed_dispatch_action_has_a_palette_row() {
        let state = AppState::test_new();
        let rows = crate::app::settings_keybindings::built_in_keybinding_entries()
            .filter_map(|(_, field, _, _)| action_for_field(field))
            .collect::<Vec<_>>();

        let intentional_dispatch_exclusions = [
            // Legacy and canonical work-link fields share aliases in the
            // dispatch order, so the palette exposes the legacy action name.
            (NavigateAction::OpenWorkUrl, "legacy open work URL alias"),
            (NavigateAction::CopyWorkUrl, "legacy copy work URL alias"),
            (
                NavigateAction::OpenWorkLink,
                "canonical field shares the legacy dispatch slot",
            ),
            (
                NavigateAction::CopyWorkLink,
                "canonical field shares the legacy dispatch slot",
            ),
        ];
        let intentional_field_exclusions = [
            // Navigate-mode workspace motion is handled directly by the
            // navigate handler, not by a NavigateAction.
            ("navigate_workspace_up", "navigate-mode workspace motion"),
            ("navigate_workspace_down", "navigate-mode workspace motion"),
            // Navigate-mode pane motion is handled directly by the navigate
            // handler, not by a palette action.
            ("navigate_pane_left", "navigate-mode pane motion"),
            ("navigate_pane_down", "navigate-mode pane motion"),
            ("navigate_pane_up", "navigate-mode pane motion"),
            ("navigate_pane_right", "navigate-mode pane motion"),
            // Indexed actions carry a parameter and have no single palette
            // command to expose.
            ("switch_workspace", "parametric indexed action"),
            ("switch_tab", "parametric indexed action"),
            ("focus_agent", "parametric indexed action"),
        ];

        for (field, reason) in intentional_field_exclusions {
            assert!(action_for_field(field).is_none(), "{field} is {reason}");
        }

        for action in non_indexed_navigation_actions_for_test(&state.keybinds) {
            if intentional_dispatch_exclusions
                .iter()
                .any(|(excluded, _)| *excluded == action)
            {
                continue;
            }
            assert!(
                rows.contains(&action),
                "dispatch action {action:?} has no BUILT_IN_GROUPS row"
            );
        }
    }

    #[test]
    fn selection_wraps_at_both_ends() {
        let mut state = AppState::test_new();
        let count = state.command_palette_entries().len();
        state.command_palette.selected = 0;
        state.move_command_palette_selection(-1);
        assert_eq!(state.command_palette.selected, count - 1);
        state.move_command_palette_selection(1);
        assert_eq!(state.command_palette.selected, 0);
    }

    #[test]
    fn no_match_clears_selection_and_selected_command() {
        let mut state = AppState::test_new();
        state.command_palette.query = "no such command".to_string();
        state.command_palette.selected = 9;
        state.clamp_command_palette_selection();
        assert!(state.command_palette_entries().is_empty());
        assert_eq!(state.command_palette.selected, 0);
        assert_eq!(state.selected_command_palette_command(), None);
    }

    #[test]
    fn unbound_commands_have_an_empty_key_hint() {
        let mut state = AppState::test_new();
        state.keybinds.help = ActionKeybinds::default();
        let entry = state
            .command_palette_entries()
            .into_iter()
            .find(|entry| entry.label == "keybinds")
            .expect("help command");
        assert!(entry.key.is_empty());
        assert_ne!(entry.key, crate::app::settings_keybindings::UNSET);
    }

    #[test]
    fn built_in_defaults_stay_aligned_with_real_dispatch() {
        let state = AppState::test_new();
        for (_group, field, _label, accessor) in built_in_keybinding_entries() {
            let Some(expected) = action_for_field(field) else {
                continue;
            };
            let bindings = accessor(&state.keybinds);
            for binding in &bindings.bindings {
                let (code, modifiers) = binding.trigger.combo();
                let actual = action_for_key_for_test(
                    &state,
                    TerminalKey::new(code, modifiers),
                    if binding.trigger.is_direct() {
                        BindingDispatch::Direct
                    } else {
                        BindingDispatch::Prefix
                    },
                );
                assert_eq!(actual, Some(expected), "dispatch drift for {field}");
            }
        }
    }

    #[test]
    fn default_command_palette_bindings_dispatch_and_parse() {
        let state = AppState::test_new();
        assert_eq!(
            action_for_key_for_test(
                &state,
                TerminalKey::new(
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                ),
                BindingDispatch::Direct,
            ),
            Some(NavigateAction::OpenCommandPalette)
        );

        let super_binding = ActionKeybinds::direct("super+p");
        assert!(super_binding
            .matches_direct_key(&TerminalKey::new(KeyCode::Char('p'), KeyModifiers::SUPER,)));
    }

    #[test]
    fn opening_the_palette_resets_its_query() {
        let mut state = AppState::test_new();
        state.command_palette.query = "split".to_string();
        state.command_palette.selected = 3;
        state.open_command_palette();
        assert_eq!(state.server_mode(), Mode::CommandPalette);
        assert_eq!(state.command_palette, CommandPaletteState::default());
    }

    #[test]
    fn accepting_a_selection_closes_before_dispatch_and_clears_query() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.open_command_palette();
        app.state.command_palette.query = "keybinds".to_string();
        app.state.command_palette.selected = 0;

        app.accept_command_palette_selection();

        assert_eq!(app.state.server_mode(), Mode::KeybindHelp);
        assert_eq!(app.state.command_palette, CommandPaletteState::default());
        app.state.open_command_palette();
        assert!(app.state.command_palette.query.is_empty());
    }
}
