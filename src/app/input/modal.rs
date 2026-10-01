use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(test)]
use ratatui::layout::Direction;
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{
            AppState, ClientOverlay, ContextMenuAction, ContextMenuKind, ContextMenuState,
            MenuListState, Mode, NavigatorStateFilter,
        },
        App,
    },
    input::TerminalKey,
    layout::NavDirection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModalAction {
    Continue,
    Save,
    Clear,
    Cancel,
    Confirm,
    Apply,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModalKeyBinding {
    Enter,
    Esc,
    CtrlC,
}

impl ModalKeyBinding {
    fn matches(self, key: &KeyEvent) -> bool {
        match self {
            Self::Enter => key.code == KeyCode::Enter,
            Self::Esc => key.code == KeyCode::Esc,
            Self::CtrlC => {
                key.code == KeyCode::Char('c')
                    && key.modifiers == crossterm::event::KeyModifiers::CONTROL
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ModalActionSpec<A> {
    pub action: A,
    pub bindings: &'static [ModalKeyBinding],
}

pub(super) fn modal_action_from_key<A: Copy>(
    key: &KeyEvent,
    specs: &[ModalActionSpec<A>],
) -> Option<A> {
    specs
        .iter()
        .find(|spec| spec.bindings.iter().any(|binding| binding.matches(key)))
        .map(|spec| spec.action)
}

pub(super) fn modal_action_from_buttons<A: Copy>(
    col: u16,
    row: u16,
    buttons: &[(Rect, A)],
) -> Option<A> {
    buttons.iter().find_map(|(rect, action)| {
        (col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height)
            .then_some(*action)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalMenuAction {
    Detach,
    WhatsNew,
    Keybinds,
    ReloadConfig,
    Settings,
}

pub(super) fn global_menu_actions(state: &AppState) -> Vec<GlobalMenuAction> {
    let mut actions = vec![
        GlobalMenuAction::Settings,
        GlobalMenuAction::Keybinds,
        GlobalMenuAction::ReloadConfig,
    ];
    if state.update_available.is_some() || state.latest_release_notes_available {
        actions.push(GlobalMenuAction::WhatsNew);
    }
    actions.push(GlobalMenuAction::Detach);
    actions
}

pub(super) fn open_global_menu(state: &mut AppState) {
    state.global_menu = MenuListState::new(0);
    // The launcher sits in the sidebar header, so the click that opens the
    // menu also focuses the sidebar. `input_owner` answers `Sidebar` ahead of
    // server modes, which left the menu undrawn and `Mode::GlobalMenu` stuck,
    // blocking Settings until restart.
    state.release_sidebar_focus_to_surface();
    state.set_server_mode(Mode::GlobalMenu);
}

pub(crate) fn handle_git_menu_key(state: &mut AppState, key: KeyEvent) {
    let in_git_repo = crate::ui::dock::chooser::focused_in_git_repo(state);
    match key.code {
        KeyCode::Esc => state.set_server_mode(Mode::Terminal),
        KeyCode::Up | KeyCode::Char('k') if in_git_repo => state.git_menu.move_prev(),
        KeyCode::Down | KeyCode::Char('j') if in_git_repo => state
            .git_menu
            .move_next(crate::app::state::GitAction::ALL.len()),
        KeyCode::Enter if in_git_repo => {
            state.request_git_action = crate::app::state::GitAction::ALL
                .get(state.git_menu.highlighted)
                .copied();
            state.set_server_mode(Mode::Terminal);
        }
        _ => {}
    }
}

pub(super) fn open_keybind_help(state: &mut AppState) {
    state.keybind_help.scroll = 0;
    state.keybind_help.query.clear();
    state.keybind_help.search_focused = false;
    state.set_server_mode(Mode::KeybindHelp);
}

fn open_update_release_notes(state: &mut AppState) {
    let Some(notes) = crate::release_notes::load_latest() else {
        return;
    };

    state.release_notes = Some(crate::app::state::ReleaseNotesState {
        version: notes.version,
        body: notes.body,
        scroll: 0,
        preview: notes.preview,
    });
    state.set_server_mode(Mode::ReleaseNotes);
}

pub(super) fn request_detach(state: &mut AppState) {
    if state.detach_exits {
        state.should_quit = true;
    } else {
        state.detach_requested = true;
    }
}

pub(super) fn apply_global_menu_action(state: &mut AppState, action: GlobalMenuAction) {
    match action {
        GlobalMenuAction::Detach => {
            leave_modal(state);
            request_detach(state);
        }
        GlobalMenuAction::WhatsNew => open_update_release_notes(state),
        GlobalMenuAction::Keybinds => open_keybind_help(state),
        GlobalMenuAction::ReloadConfig => {
            state.request_reload_config = true;
            leave_modal(state);
        }
        GlobalMenuAction::Settings => super::settings::open_settings(state),
    }
}

pub(crate) fn handle_global_menu_key(state: &mut AppState, key: KeyEvent) {
    let actions = global_menu_actions(state);
    match key.code {
        KeyCode::Esc => leave_modal(state),
        KeyCode::Up | KeyCode::Char('k') => state.global_menu.move_prev(),
        KeyCode::Down | KeyCode::Char('j') => state.global_menu.move_next(actions.len()),
        KeyCode::Enter => {
            if let Some(action) = actions.get(state.global_menu.highlighted).copied() {
                apply_global_menu_action(state, action);
            }
        }
        _ => {}
    }
}

pub(crate) fn handle_navigator_key(
    state: &mut AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    key: KeyEvent,
) {
    if state.navigator.search_focused {
        match key.code {
            KeyCode::Esc => {
                state.navigator.search_focused = false;
            }
            KeyCode::Enter => {
                state.accept_navigator_selection_from(terminal_runtimes);
            }
            KeyCode::Backspace => {
                state.navigator.state_filter = None;
                state.navigator.query.pop();
                state.select_first_navigator_match_from(terminal_runtimes);
            }
            KeyCode::Up => state.move_navigator_selection_from(terminal_runtimes, -1),
            KeyCode::Down => state.move_navigator_selection_from(terminal_runtimes, 1),
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                state.move_navigator_selection_from(terminal_runtimes, 1)
            }
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                state.move_navigator_selection_from(terminal_runtimes, -1)
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                state.navigator.query.clear();
                state.navigator.state_filter = None;
                state.clamp_navigator_selection_from(terminal_runtimes);
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                insert_navigator_search_text(state, terminal_runtimes, &c.to_string());
            }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Esc => {
            leave_modal(state);
        }
        KeyCode::Enter => {
            state.accept_navigator_selection_from(terminal_runtimes);
        }
        KeyCode::Char('/') => {
            state.navigator.state_filter = None;
            state.navigator.search_focused = true;
            state.clamp_navigator_selection_from(terminal_runtimes);
        }
        KeyCode::Backspace if state.navigator.state_filter.is_some() => {
            state.navigator.state_filter = None;
            state.clamp_navigator_selection_from(terminal_runtimes);
        }
        KeyCode::Char('a') if key.modifiers.is_empty() => {
            state.navigator.query.clear();
            state.navigator.state_filter = None;
            state.clamp_navigator_selection_from(terminal_runtimes);
        }
        KeyCode::Char('b') if key.modifiers.is_empty() => {
            state.navigator.query.clear();
            state.navigator.state_filter = Some(NavigatorStateFilter::Blocked);
            state.select_first_navigator_match_from(terminal_runtimes);
        }
        KeyCode::Char('w') if key.modifiers.is_empty() => {
            state.navigator.query.clear();
            state.navigator.state_filter = Some(NavigatorStateFilter::Working);
            state.select_first_navigator_match_from(terminal_runtimes);
        }
        KeyCode::Char('i') if key.modifiers.is_empty() => {
            state.navigator.query.clear();
            state.navigator.state_filter = Some(NavigatorStateFilter::Idle);
            state.select_first_navigator_match_from(terminal_runtimes);
        }
        KeyCode::Char('d') if key.modifiers.is_empty() => {
            state.navigator.query.clear();
            state.navigator.state_filter = Some(NavigatorStateFilter::Done);
            state.select_first_navigator_match_from(terminal_runtimes);
        }
        KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
            state.move_navigator_selection_from(terminal_runtimes, 1)
        }
        KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
            state.move_navigator_selection_from(terminal_runtimes, -1)
        }
        KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => state
            .move_navigator_selection_by_lines_from(
                terminal_runtimes,
                (state.navigator_body_rect().height / 2).max(1) as isize,
            ),
        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => state
            .move_navigator_selection_by_lines_from(
                terminal_runtimes,
                -((state.navigator_body_rect().height / 2).max(1) as isize),
            ),
        KeyCode::Char(' ') => state.toggle_selected_navigator_workspace_from(terminal_runtimes),
        KeyCode::Home => {
            state.navigator.selected = 0;
            state.ensure_navigator_selection_visible_from(terminal_runtimes);
        }
        KeyCode::End | KeyCode::Char('G') => {
            state.navigator.selected = state
                .navigator_rows_from(terminal_runtimes)
                .len()
                .saturating_sub(1);
            state.ensure_navigator_selection_visible_from(terminal_runtimes);
        }
        _ => {}
    }
}

pub(crate) fn insert_navigator_search_text(
    state: &mut AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    text: &str,
) {
    if !state.navigator.search_focused {
        return;
    }
    state.navigator.state_filter = None;
    state.navigator.query.push_str(text);
    state.select_first_navigator_match_from(terminal_runtimes);
}

pub(crate) fn insert_keybind_help_query_text(state: &mut AppState, text: &str) {
    if !state.keybind_help.search_focused {
        return;
    }
    state
        .keybind_help
        .query
        .extend(text.chars().filter(|ch| !ch.is_control()));
    state.keybind_help.scroll = 0;
}

pub(super) fn keybind_help_back(state: &mut AppState) {
    if state.keybind_help.search_focused {
        state.keybind_help.query.clear();
        state.keybind_help.search_focused = false;
        state.keybind_help.scroll = 0;
    } else {
        leave_modal(state);
    }
}

pub(crate) fn handle_keybind_help_key(state: &mut AppState, key: TerminalKey) {
    if state.keybind_help.search_focused {
        let text_char = keybind_help_text_char(key.clone());
        match key.code {
            KeyCode::Up => state.scroll_keybind_help(-1),
            KeyCode::Down => state.scroll_keybind_help(1),
            KeyCode::PageUp => state.scroll_keybind_help(-8),
            KeyCode::PageDown => state.scroll_keybind_help(8),
            KeyCode::Home => state.keybind_help.scroll = 0,
            KeyCode::End => state.keybind_help.scroll = state.keybind_help_max_scroll(),
            KeyCode::Backspace => {
                state.keybind_help.query.pop();
                state.keybind_help.scroll = 0;
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                state.keybind_help.query.clear();
                state.keybind_help.scroll = 0;
            }
            KeyCode::Esc => keybind_help_back(state),
            KeyCode::Enter => leave_modal(state),
            _ => {
                if let Some(character) = text_char {
                    insert_keybind_help_query_text(state, &character.to_string());
                }
            }
        }
        return;
    }

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.scroll_keybind_help(-1),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_keybind_help(1),
        KeyCode::PageUp => state.scroll_keybind_help(-8),
        KeyCode::PageDown => state.scroll_keybind_help(8),
        KeyCode::Home => state.keybind_help.scroll = 0,
        KeyCode::End => state.keybind_help.scroll = state.keybind_help_max_scroll(),
        _ if keybind_help_text_char(key.clone()) == Some('/') => {
            state.keybind_help.search_focused = true;
            state.keybind_help.scroll = 0;
        }
        KeyCode::Esc => keybind_help_back(state),
        KeyCode::Enter => leave_modal(state),
        _ if keybind_help_text_char(key.clone()) == Some('?') => leave_modal(state),
        _ => {}
    }
}

fn keybind_help_text_char(key: TerminalKey) -> Option<char> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    if let Some(character) = key.shifted_codepoint.and_then(char::from_u32) {
        return Some(character);
    }
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    Some(character)
}

pub(super) fn open_rename_workspace(
    state: &mut AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ws_idx: usize,
) {
    let Some(workspace_id) = state.workspaces.get(ws_idx).map(|ws| ws.id.clone()) else {
        return;
    };
    state.pending_workspace_create_cwd = None;
    state.selected = ws_idx;
    state.rename_pane_target = None;
    state.rename_target = Some(crate::app::state::RenameTarget::Workspace { workspace_id });
    state.name_input =
        state.workspaces[ws_idx].display_name_from(&state.terminals, terminal_runtimes);
    state.name_input_replace_on_type = false;
    state.open_client_overlay(ClientOverlay::RenameWorkspace);
}

pub(crate) fn open_rename_pod(state: &mut AppState, record: crate::groups::GroupRecord) {
    let crate::groups::GroupState::Active { name } = &record.state else {
        return;
    };
    state.pending_workspace_create_cwd = None;
    state.rename_pane_target = None;
    state.rename_target = Some(crate::app::state::RenameTarget::Pod {
        record: record.clone(),
    });
    state.name_input = name.clone();
    state.name_input_replace_on_type = false;
    state.open_client_overlay(ClientOverlay::RenamePane);
}

pub(crate) fn open_new_workspace_dialog(state: &mut AppState, cwd: std::path::PathBuf) {
    let suggested_name = crate::workspace::derive_label_from_cwd(&cwd);
    state.creating_new_tab = false;
    state.requested_new_tab_name = None;
    state.pending_workspace_create_cwd = Some(cwd);
    state.rename_pane_target = None;
    state.rename_target = None;
    state.name_input = suggested_name;
    state.name_input_replace_on_type = true;
    state.open_client_overlay(ClientOverlay::RenameWorkspace);
}

pub(super) fn open_rename_active_tab(state: &mut AppState, replace_on_type: bool) {
    state.creating_new_tab = false;
    state.requested_new_tab_name = None;
    state.pending_workspace_create_cwd = None;
    state.rename_pane_target = None;
    state.rename_target = None;
    state.rename_tab_prefill = None;
    if let Some(ws) = state.active.and_then(|i| state.workspaces.get(i)) {
        let workspace_id = ws.id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace_id,
            ws.tabs[ws.active_tab].number,
        );
        let prefill = ws
            .active_tab_display_name_from(&state.terminals)
            .unwrap_or_else(|| (ws.active_tab + 1).to_string());
        state.rename_tab_prefill = Some(prefill.clone());
        state.name_input = prefill;
        state.name_input_replace_on_type = replace_on_type;
        state.rename_target = Some(crate::app::state::RenameTarget::Tab {
            workspace_id,
            tab_id,
        });
        state.open_client_overlay(ClientOverlay::RenameTab);
    }
}

pub(super) fn open_rename_pane(state: &mut AppState, pane_id: crate::layout::PaneId) {
    let Some(workspace_id) = state
        .active
        .and_then(|i| state.workspaces.get(i))
        .map(|workspace| workspace.id.clone())
    else {
        return;
    };
    open_rename_pane_in_workspace(state, &workspace_id, pane_id);
}

fn open_rename_pane_in_workspace(
    state: &mut AppState,
    workspace_id: &str,
    pane_id: crate::layout::PaneId,
) {
    let Some(ws) = state
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
    else {
        return;
    };
    let Some(pane) = ws.pane_state(pane_id) else {
        return;
    };
    let terminal = state.terminals.get(&pane.attached_terminal_id);
    state.creating_new_tab = false;
    state.requested_new_tab_name = None;
    state.pending_workspace_create_cwd = None;
    state.rename_pane_target = Some(pane_id);
    state.rename_target = Some(crate::app::state::RenameTarget::Pane {
        workspace_id: workspace_id.to_string(),
        pane_id,
    });
    state.name_input = terminal
        .and_then(|t| t.manual_label.clone())
        .unwrap_or_default();
    state.name_input_replace_on_type = terminal.and_then(|t| t.manual_label.as_ref()).is_none();
    state.open_client_overlay(ClientOverlay::RenamePane);
}

fn workspace_create_label(input: &str, suggested_name: &str) -> Option<String> {
    let name = input.trim();
    (!name.is_empty() && name != suggested_name).then(|| name.to_string())
}

fn next_new_tab_default_name(state: &AppState) -> String {
    state
        .active
        .and_then(|i| state.workspaces.get(i))
        .map(|ws| (ws.tabs.len() + 1).to_string())
        .unwrap_or_else(|| "1".to_string())
}

pub(crate) fn open_new_tab_dialog(state: &mut AppState) {
    state.creating_new_tab = true;
    state.requested_new_tab_name = None;
    state.pending_workspace_create_cwd = None;
    state.rename_pane_target = None;
    state.rename_target = None;
    state.name_input = next_new_tab_default_name(state);
    state.name_input_replace_on_type = true;
    state.open_client_overlay(ClientOverlay::RenameTab);
}

pub(super) fn leave_modal(state: &mut AppState) {
    if state.client_overlay != ClientOverlay::None {
        state.close_client_overlay();
    } else if state.active.is_some() {
        state.set_server_mode(Mode::Terminal);
    } else {
        state.set_server_mode(Mode::Navigate);
    }
}

pub(super) const ONBOARDING_WELCOME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Continue,
    bindings: &[ModalKeyBinding::Enter],
}];

pub(super) const RELEASE_NOTES_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Close,
    bindings: &[ModalKeyBinding::Enter, ModalKeyBinding::Esc],
}];

pub(super) const RENAME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Save,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Clear,
        bindings: &[ModalKeyBinding::CtrlC],
    },
    ModalActionSpec {
        action: ModalAction::Cancel,
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) const CONFIRM_CLOSE_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Confirm,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Cancel,
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) const SETTINGS_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Apply,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Close,
        bindings: &[ModalKeyBinding::Esc],
    },
];

#[cfg(test)]
pub(super) fn apply_rename_action(state: &mut AppState, action: ModalAction) {
    match action {
        ModalAction::Save => {
            let new_name = if state.name_input.trim().is_empty() {
                state.name_input.clone()
            } else {
                state.name_input.trim().to_string()
            };
            match state.effective_interaction_mode() {
                Mode::RenameWorkspace
                    if state.pending_workspace_create_cwd.is_none()
                        && !state.workspaces.is_empty()
                        && !new_name.is_empty() =>
                {
                    let ws_idx = match state.rename_target.as_ref() {
                        Some(crate::app::state::RenameTarget::Workspace { workspace_id }) => state
                            .workspaces
                            .iter()
                            .position(|workspace| workspace.id == *workspace_id),
                        _ => Some(state.selected),
                    };
                    let Some(ws_idx) = ws_idx else {
                        cancel_rename_modal(state);
                        return;
                    };
                    let workspace_id = state.workspaces[ws_idx].id.clone();
                    state.workspaces[ws_idx].set_custom_name(new_name);
                    crate::logging::workspace_renamed(&workspace_id);
                    state.mark_session_dirty();
                }
                Mode::RenameTab if state.creating_new_tab => {
                    state.request_new_tab = true;
                    let default_name = next_new_tab_default_name(state);
                    state.requested_new_tab_name =
                        if new_name.is_empty() || new_name == default_name {
                            None
                        } else {
                            Some(new_name)
                        };
                }
                Mode::RenameTab => {
                    let target = state.rename_target.clone().and_then(|target| match target {
                        crate::app::state::RenameTarget::Tab {
                            workspace_id,
                            tab_id,
                        } => {
                            let ws_idx = state
                                .workspaces
                                .iter()
                                .position(|workspace| workspace.id == *workspace_id)?;
                            let tab_idx = state.workspaces[ws_idx].tabs.iter().position(|tab| {
                                crate::workspace::public_tab_id_for_number(
                                    &workspace_id,
                                    tab.number,
                                ) == *tab_id
                            })?;
                            Some((ws_idx, tab_idx))
                        }
                        _ => None,
                    });
                    if let Some((ws_idx, target_tab)) = target.or_else(|| {
                        state
                            .active
                            .map(|ws_idx| (ws_idx, state.workspaces[ws_idx].active_tab))
                    }) {
                        let prefill = state.rename_tab_prefill.take();
                        if let Some(ws) = state.workspaces.get_mut(ws_idx) {
                            let workspace_id = ws.id.clone();
                            let active_tab = target_tab;
                            // Compare against what the modal was opened with, not a freshly
                            // derived label: the live label can advance while the modal is open,
                            // and an unedited Enter must stay a no-op.
                            let current_name = prefill.unwrap_or_else(|| {
                                ws.tab_display_name_from(&state.terminals, active_tab)
                                    .unwrap_or_else(|| (active_tab + 1).to_string())
                            });
                            let keep_auto_name = ws
                                .tabs
                                .get(active_tab)
                                .is_some_and(|tab| tab.is_auto_named())
                                && new_name == current_name;
                            if let Some(tab) = ws.tabs.get_mut(active_tab) {
                                if !new_name.is_empty() && !keep_auto_name {
                                    tab.set_user_custom_name(new_name);
                                    let tab_id = ws
                                        .public_tab_number(active_tab)
                                        .map(|number| {
                                            crate::workspace::public_tab_id_for_number(
                                                &workspace_id,
                                                number,
                                            )
                                        })
                                        .unwrap_or_else(|| workspace_id.clone());
                                    crate::logging::tab_renamed(&workspace_id, &tab_id);
                                    state.mark_session_dirty();
                                }
                            }
                        }
                    }
                }
                Mode::RenamePane => {
                    let target = state.rename_target.clone().and_then(|target| match target {
                        crate::app::state::RenameTarget::Pane {
                            workspace_id,
                            pane_id,
                        } => state
                            .workspaces
                            .iter()
                            .position(|workspace| workspace.id == workspace_id)
                            .map(|ws_idx| (ws_idx, pane_id)),
                        _ => None,
                    });
                    if let Some((ws_idx, pane_id)) =
                        target.or_else(|| state.active.zip(state.rename_pane_target))
                    {
                        if let Some(ws) = state.workspaces.get(ws_idx) {
                            if let Some(pane) = ws.pane_state(pane_id) {
                                let terminal_id = pane.attached_terminal_id.clone();
                                if let Some(terminal) = state.terminals.get_mut(&terminal_id) {
                                    terminal.set_manual_label(new_name);
                                    state.mark_session_dirty();
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            state.creating_new_tab = false;
            state.pending_workspace_create_cwd = None;
            state.rename_pane_target = None;
            state.rename_target = None;
            state.name_input.clear();
            state.name_input_replace_on_type = false;
            leave_modal(state);
        }
        ModalAction::Clear => {
            state.name_input.clear();
            state.name_input_replace_on_type = false;
        }
        ModalAction::Cancel => {
            state.creating_new_tab = false;
            state.requested_new_tab_name = None;
            state.pending_workspace_create_cwd = None;
            state.rename_pane_target = None;
            state.rename_target = None;
            state.name_input.clear();
            state.name_input_replace_on_type = false;
            leave_modal(state);
        }
        _ => {}
    }
}

fn clear_rename_input(state: &mut AppState) {
    state.name_input.clear();
    state.name_input_replace_on_type = false;
}

pub(crate) fn insert_rename_input_text(state: &mut AppState, text: &str) {
    if state.name_input_replace_on_type {
        clear_rename_input(state);
    }
    state.name_input.push_str(text);
}

fn delete_rename_input_char(state: &mut AppState) {
    if state.name_input_replace_on_type {
        clear_rename_input(state);
    } else {
        state.name_input.pop();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameWordDeleteClass {
    Word,
    Separator,
}

fn rename_word_delete_class(ch: char) -> RenameWordDeleteClass {
    if ch.is_alphanumeric() || ch == '_' {
        RenameWordDeleteClass::Word
    } else {
        RenameWordDeleteClass::Separator
    }
}

fn delete_rename_input_word(state: &mut AppState) {
    if state.name_input_replace_on_type {
        clear_rename_input(state);
        return;
    }

    while state
        .name_input
        .chars()
        .last()
        .is_some_and(char::is_whitespace)
    {
        state.name_input.pop();
    }

    let Some(class) = state
        .name_input
        .chars()
        .last()
        .map(rename_word_delete_class)
    else {
        return;
    };

    while state
        .name_input
        .chars()
        .last()
        .is_some_and(|ch| !ch.is_whitespace() && rename_word_delete_class(ch) == class)
    {
        state.name_input.pop();
    }
}

fn handle_rename_edit_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            clear_rename_input(state);
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::SUPER) => {
            clear_rename_input(state);
        }
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            delete_rename_input_word(state);
        }
        KeyCode::Char('h' | 'w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            delete_rename_input_word(state);
        }
        KeyCode::Backspace => delete_rename_input_char(state),
        KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            insert_rename_input_text(state, &c.to_string());
        }
        _ => {}
    }
}

fn delete_snooze_input_word(draft: &mut String) {
    while draft.chars().last().is_some_and(char::is_whitespace) {
        draft.pop();
    }
    let Some(class) = draft.chars().last().map(rename_word_delete_class) else {
        return;
    };
    while draft
        .chars()
        .last()
        .is_some_and(|ch| !ch.is_whitespace() && rename_word_delete_class(ch) == class)
    {
        draft.pop();
    }
}

fn handle_snooze_time_edit_key(draft: &mut String, key: KeyEvent) {
    match key.code {
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => draft.clear(),
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::SUPER) => draft.clear(),
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            delete_snooze_input_word(draft);
        }
        KeyCode::Char('h' | 'w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            delete_snooze_input_word(draft);
        }
        KeyCode::Backspace => {
            draft.pop();
        }
        KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            draft.push(c);
        }
        _ => {}
    }
}

#[cfg(test)]
pub(crate) fn handle_rename_key(state: &mut AppState, key: KeyEvent) {
    if let Some(action) = modal_action_from_key(&key, RENAME_ACTIONS) {
        apply_rename_action(state, action);
        return;
    }

    handle_rename_edit_key(state, key);
}

#[cfg(test)]
pub(crate) fn handle_resize_key(state: &mut AppState, raw_key: TerminalKey) {
    let key = raw_key.as_key_event();
    if key.code == KeyCode::Esc
        || key.code == KeyCode::Enter
        || state.keybinds.resize_mode.matches_prefix_key(&raw_key)
        || state.keybinds.resize_mode.matches_direct_key(&raw_key)
    {
        if state.active.is_some() {
            state.set_server_mode(Mode::Terminal);
        } else {
            state.set_server_mode(Mode::Navigate);
        }
        return;
    }

    match key.code {
        KeyCode::Char('h') | KeyCode::Left => state.resize_pane(NavDirection::Left),
        KeyCode::Char('l') | KeyCode::Right => state.resize_pane(NavDirection::Right),
        KeyCode::Char('j') | KeyCode::Down => state.resize_pane(NavDirection::Down),
        KeyCode::Char('k') | KeyCode::Up => state.resize_pane(NavDirection::Up),
        _ => {}
    }
}

pub(super) fn open_confirm_close(state: &mut AppState) {
    state.begin_workspace_close_confirmation(state.selected);
}

#[cfg(test)]
pub(super) fn confirm_close_accept(state: &mut AppState) {
    if let Some(ws_idx) = state.take_confirmed_workspace_close_index() {
        state.selected = ws_idx;
        state.close_selected_workspace();
    }
    state.close_client_overlay();
}

pub(super) fn confirm_close_cancel(state: &mut AppState) {
    state.confirm_close_workspace_id = None;
    state.close_client_overlay();
}

#[cfg(test)]
pub(crate) fn handle_confirm_close_key(state: &mut AppState, key: KeyEvent) {
    match modal_action_from_key(&key, CONFIRM_CLOSE_ACTIONS) {
        Some(ModalAction::Confirm) => confirm_close_accept(state),
        Some(ModalAction::Cancel) => confirm_close_cancel(state),
        _ => {}
    }
}

#[cfg(test)]
pub(super) fn apply_context_menu_action(
    state: &mut AppState,
    terminal_runtimes: &mut crate::terminal::TerminalRuntimeRegistry,
    mut menu: ContextMenuState,
    action: ContextMenuAction,
) {
    if !state.rebase_context_menu_indices(&mut menu) {
        leave_modal(state);
        return;
    }
    let actions = state.context_menu_actions(&menu);
    let item = actions
        .iter()
        .position(|candidate| *candidate == action)
        .and_then(|idx| state.context_menu_items(&menu).get(idx).copied());
    let (menu_x, menu_y) = (menu.x, menu.y);
    match (menu.kind, item) {
        (ContextMenuKind::GitWorkspace { ws_idx, .. }, Some("New worktree")) => {
            state.request_new_linked_worktree = Some(ws_idx);
            leave_modal(state);
        }
        (ContextMenuKind::GitWorkspace { ws_idx, .. }, Some("Delete worktree checkout...")) => {
            state.request_remove_linked_worktree = Some(ws_idx);
            leave_modal(state);
        }
        (ContextMenuKind::GitWorkspace { ws_idx, .. }, Some("Open worktree...")) => {
            state.request_open_existing_worktree = Some(ws_idx);
            leave_modal(state);
        }
        (
            ContextMenuKind::GitWorkspace {
                ws_idx, collapsed, ..
            },
            Some("Collapse" | "Expand"),
        ) => {
            // The key has to be the one the sidebar reads back, which is the
            // group key, not the raw worktree key: a Space grouped by repository
            // has no worktree membership and would silently collapse nothing.
            if let Some((key, _)) = crate::ui::workspace_parent_group_state(state, ws_idx) {
                if collapsed {
                    state.collapsed_space_keys.remove(&key);
                } else {
                    state.collapsed_space_keys.insert(key);
                }
                state.mark_session_dirty();
            }
            leave_modal(state);
        }
        (
            ContextMenuKind::Workspace { ws_idx, .. }
            | ContextMenuKind::GitWorkspace { ws_idx, .. },
            Some("Rename"),
        ) => {
            open_rename_workspace(state, terminal_runtimes, ws_idx);
        }
        (
            ContextMenuKind::Workspace { ws_idx, .. }
            | ContextMenuKind::GitWorkspace { ws_idx, .. },
            Some("Close" | "Close group"),
        ) => {
            state.selected = ws_idx;
            if state.confirm_close {
                open_confirm_close(state);
            } else {
                state.close_selected_workspace();
                state.set_server_mode(Mode::Navigate);
            }
        }
        (
            ContextMenuKind::Tab {
                ws_idx, tab_idx, ..
            },
            Some("New tab"),
        ) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            open_new_tab_dialog(state);
        }
        (
            ContextMenuKind::Tab {
                ws_idx, tab_idx, ..
            },
            Some("Rename"),
        ) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            open_rename_active_tab(state, false);
        }
        (
            ContextMenuKind::Tab {
                ws_idx, tab_idx, ..
            },
            Some(crate::app::state::STAR_ITEM | crate::app::state::UNSTAR_ITEM),
        ) => {
            if let Some(tab) = state
                .workspaces
                .get_mut(ws_idx)
                .and_then(|ws| ws.tabs.get_mut(tab_idx))
            {
                tab.starred = !tab.starred;
                state.mark_session_dirty();
            }
            state.set_server_mode(if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            });
        }
        (
            ContextMenuKind::Tab {
                ws_idx, tab_idx, ..
            },
            Some(crate::app::state::MOVE_TO_SUBGROUP_ITEM),
        ) => {
            // The picker hangs off the menu cell the operator just chose, the
            // way every other downward menu hangs off its anchor.
            state.sidebar_subgroup_picker = Some(crate::app::state::SidebarSubgroupPickerState {
                ws_idx,
                tab_idx,
                anchor: (menu_x, menu_y),
                filter: crate::ui::dropdown::DropdownFilterState::default(),
            });
            state.set_server_mode(if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            });
        }
        (
            ContextMenuKind::Tab {
                ws_idx, tab_idx, ..
            },
            Some(crate::app::state::REMOVE_FROM_SUBGROUP_ITEM),
        ) => {
            state.clear_tab_subgroup(ws_idx, tab_idx);
            state.set_server_mode(if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            });
        }
        (
            ContextMenuKind::Tab {
                ws_idx,
                settle_pane_id: Some(pane_id),
                ..
            },
            Some(crate::app::state::SETTLE_ITEM),
        ) => {
            state.settle_pane_at(
                ws_idx,
                pane_id,
                crate::app::settled::unix_seconds(std::time::SystemTime::now()),
            );
            leave_modal(state);
        }
        (
            ContextMenuKind::Tab {
                ws_idx, tab_idx, ..
            },
            Some("Close"),
        ) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            if !state.close_tab() {
                state.set_server_mode(if state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                });
            }
        }
        (
            ContextMenuKind::Pane {
                workspace_id,
                pane_id,
                ..
            },
            Some("Rename pane"),
        ) => {
            open_rename_pane_in_workspace(state, &workspace_id, pane_id);
        }
        (
            ContextMenuKind::Pane {
                ws_idx,
                tab_idx,
                linkable_work_link: Some(action),
                ..
            },
            Some(item),
        ) if item == action.menu_item() => {
            let patch = action.patch();
            for pane_id in state.window_pane_ids(ws_idx, tab_idx) {
                let Some(terminal_id) = state
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.pane_state(pane_id))
                    .map(|pane| pane.attached_terminal_id.clone())
                else {
                    continue;
                };
                let Some(terminal) = state.terminals.get_mut(&terminal_id) else {
                    continue;
                };
                match terminal.apply_manual_work_context_patch(patch.clone()) {
                    Ok(true) => state.mark_session_dirty(),
                    Ok(false) => {}
                    Err(err) => tracing::warn!(err = %err, "failed to link work item to window"),
                }
            }
            state.set_server_mode(Mode::Terminal);
        }
        (
            ContextMenuKind::Pane {
                link: Some(link), ..
            },
            Some(crate::app::state::COPY_LINK_ITEM),
        ) => {
            state.request_clipboard_write = Some(link.into_bytes());
            state.set_server_mode(Mode::Terminal);
        }
        (
            ContextMenuKind::Pane {
                ws_idx, pane_id, ..
            },
            Some("Clear pane name"),
        ) => {
            if let Some(ws) = state.workspaces.get(ws_idx) {
                if let Some(pane) = ws.pane_state(pane_id) {
                    let terminal_id = pane.attached_terminal_id.clone();
                    if let Some(terminal) = state.terminals.get_mut(&terminal_id) {
                        terminal.clear_manual_label();
                        state.mark_session_dirty();
                    }
                }
            }
            state.set_server_mode(Mode::Terminal);
        }
        (
            ContextMenuKind::Pane {
                ws_idx,
                tab_idx,
                pane_id,
                source_pane_id,
                ..
            },
            Some("Swap with focused pane"),
        ) => {
            if let Some(source_pane_id) = source_pane_id {
                state.selected = ws_idx;
                state.active = Some(ws_idx);
                state.switch_tab(tab_idx);
                if let Some(tab) = state
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|ws| ws.tabs.get_mut(tab_idx))
                {
                    if tab.layout.swap_panes(source_pane_id, pane_id) {
                        tab.layout.focus_pane(source_pane_id);
                        state.mark_session_dirty();
                    }
                }
            }
            state.set_server_mode(Mode::Terminal);
        }
        (
            ContextMenuKind::Pane {
                ws_idx,
                tab_idx,
                pane_id,
                ..
            },
            Some("Split right"),
        ) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            state.focus_pane_in_workspace(ws_idx, pane_id);
            state.split_pane(terminal_runtimes, Direction::Horizontal);
            state.set_server_mode(Mode::Terminal);
        }
        (
            ContextMenuKind::Pane {
                ws_idx,
                tab_idx,
                pane_id,
                ..
            },
            Some("Split down"),
        ) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            state.focus_pane_in_workspace(ws_idx, pane_id);
            state.split_pane(terminal_runtimes, Direction::Vertical);
            state.set_server_mode(Mode::Terminal);
        }
        (
            ContextMenuKind::Pane {
                ws_idx,
                tab_idx,
                pane_id,
                ..
            },
            Some("Zoom"),
        ) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            state.focus_pane_in_workspace(ws_idx, pane_id);
            state.toggle_zoom();
            state.set_server_mode(Mode::Terminal);
        }
        (
            ContextMenuKind::Pane {
                ws_idx,
                tab_idx,
                pane_id,
                ..
            },
            Some("Close pane"),
        ) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            state.focus_pane_in_workspace(ws_idx, pane_id);
            if !state.close_pane() {
                state.set_server_mode(if state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                });
            }
        }
        _ => leave_modal(state),
    }
}

#[cfg(test)]
pub(crate) fn handle_context_menu_key(
    state: &mut AppState,
    terminal_runtimes: &mut crate::terminal::TerminalRuntimeRegistry,
    key: KeyEvent,
) {
    match key.code {
        KeyCode::Esc => {
            state.context_menu = None;
            leave_modal(state);
        }
        KeyCode::Up => {
            if let Some(menu) = state.context_menu.as_ref() {
                let actions = state.context_menu_actions(menu);
                let selected = actions
                    .iter()
                    .position(|action| *action == menu.selected)
                    .unwrap_or(0)
                    .saturating_sub(1);
                if let (Some(menu), Some(action)) =
                    (state.context_menu.as_mut(), actions.get(selected).copied())
                {
                    menu.selected = action;
                }
            }
        }
        KeyCode::Down => {
            if let Some(menu) = state.context_menu.as_ref() {
                let actions = state.context_menu_actions(menu);
                let selected = actions
                    .iter()
                    .position(|action| *action == menu.selected)
                    .unwrap_or(0)
                    .saturating_add(1)
                    .min(actions.len().saturating_sub(1));
                if let (Some(menu), Some(action)) =
                    (state.context_menu.as_mut(), actions.get(selected).copied())
                {
                    menu.selected = action;
                }
            }
        }
        KeyCode::Enter => {
            if let Some(menu) = state.context_menu.take() {
                let action = menu.selected;
                leave_modal(state);
                apply_context_menu_action(state, terminal_runtimes, menu, action);
            }
        }
        _ => {}
    }
}

impl App {
    pub(crate) fn handle_sidebar_snooze_time_key(&mut self, key: KeyEvent) -> bool {
        if self
            .state
            .sidebar_snooze
            .as_ref()
            .is_none_or(|snooze| snooze.time_draft.is_none())
        {
            return false;
        }
        if let Some(action) = modal_action_from_key(&key, RENAME_ACTIONS) {
            match action {
                ModalAction::Save => self.save_snooze_time_modal_via_api(),
                ModalAction::Clear => {
                    if let Some(snooze) = self.state.sidebar_snooze.as_mut() {
                        if let Some(draft) = snooze.time_draft.as_mut() {
                            draft.clear();
                        }
                        snooze.error = None;
                    }
                }
                ModalAction::Cancel => self.state.sidebar_snooze = None,
                _ => {}
            }
            return true;
        }
        if let Some(snooze) = self.state.sidebar_snooze.as_mut() {
            snooze.error = None;
            if let Some(draft) = snooze.time_draft.as_mut() {
                handle_snooze_time_edit_key(draft, key);
            }
        }
        true
    }

    pub(crate) fn handle_rename_key_via_api(&mut self, key: KeyEvent) {
        if let Some(action) = modal_action_from_key(&key, RENAME_ACTIONS) {
            self.apply_rename_mouse_action_via_api(action);
            return;
        }

        handle_rename_edit_key(&mut self.state, key);
    }

    fn save_rename_modal_via_api(&mut self) {
        // A client-owned overlay may focus shared runtime state, but it cannot
        // replace the server-owned full-screen interaction another client uses.
        let server_mode = self.state.server_mode();
        let new_name = if self.state.name_input.trim().is_empty() {
            self.state.name_input.clone()
        } else {
            self.state.name_input.trim().to_string()
        };

        match self.state.effective_interaction_mode() {
            Mode::RenameWorkspace => {
                if let Some(cwd) = self.state.pending_workspace_create_cwd.take() {
                    let suggested_name = crate::workspace::derive_label_from_cwd(&cwd);
                    let label = workspace_create_label(&new_name, &suggested_name);
                    self.runtime_workspace_create(
                        "tui.workspace.create_named",
                        crate::api::schema::WorkspaceCreateParams {
                            cwd: Some(cwd.display().to_string()),
                            focus: true,
                            label,
                            env: Default::default(),
                            work_context: None,
                            source_workspace_id: None,
                        },
                    );
                } else if !new_name.is_empty() {
                    if let Some(crate::app::state::RenameTarget::Workspace { workspace_id }) =
                        self.state.rename_target.clone()
                    {
                        if self
                            .state
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.id == workspace_id)
                        {
                            self.runtime_workspace_rename(
                                "tui.workspace.rename",
                                crate::api::schema::WorkspaceRenameParams {
                                    workspace_id,
                                    label: new_name,
                                },
                            );
                        }
                    }
                }
            }
            Mode::RenameTab if self.state.creating_new_tab => {
                let default_name = next_new_tab_default_name(&self.state);
                let label = if new_name.is_empty() || new_name == default_name {
                    None
                } else {
                    Some(new_name)
                };
                self.runtime_tab_create(
                    "tui.tab.create_named",
                    crate::api::schema::TabCreateParams {
                        workspace_id: None,
                        cwd: None,
                        focus: true,
                        label,
                        env: Default::default(),
                        work_context: None,
                    },
                );
            }
            Mode::RenameTab if !new_name.is_empty() => {
                let Some(crate::app::state::RenameTarget::Tab {
                    workspace_id,
                    tab_id,
                }) = self.state.rename_target.clone()
                else {
                    cancel_rename_modal(&mut self.state);
                    return;
                };
                let Some((ws_idx, tab_idx)) = self
                    .parse_tab_id(&tab_id)
                    .filter(|(ws_idx, _)| self.state.workspaces[*ws_idx].id == workspace_id)
                else {
                    cancel_rename_modal(&mut self.state);
                    return;
                };
                // See `apply_rename_action`: the prefill is the baseline, not the live label.
                let current_name = self.state.rename_tab_prefill.take().unwrap_or_else(|| {
                    self.state.workspaces[ws_idx]
                        .tab_display_name_from(&self.state.terminals, tab_idx)
                        .unwrap_or_else(|| (tab_idx + 1).to_string())
                });
                let keep_auto_name = self.state.workspaces[ws_idx]
                    .tabs
                    .get(tab_idx)
                    .is_some_and(|tab| tab.is_auto_named())
                    && new_name == current_name;
                if !keep_auto_name {
                    self.runtime_tab_rename(
                        "tui.tab.rename",
                        crate::api::schema::TabRenameParams {
                            tab_id,
                            label: Some(new_name),
                        },
                    );
                }
            }
            Mode::RenamePane => {
                if let Some(crate::app::state::RenameTarget::Pod { record }) =
                    self.state.rename_target.clone()
                {
                    if !new_name.is_empty() {
                        let response = self.dispatch_runtime_mutation(
                            "sidebar-pod-rename",
                            crate::api::schema::Method::GroupRename(
                                crate::api::schema::GroupRenameParams {
                                    group_id: record.id,
                                    name: new_name,
                                    expected_revision: record.revision,
                                },
                            ),
                        );
                        if serde_json::from_str::<crate::api::schema::ErrorResponse>(&response)
                            .is_ok()
                        {
                            self.show_pod_mutation_error("Pod not renamed", &response);
                        }
                    }
                } else if let Some(crate::app::state::RenameTarget::Pane {
                    workspace_id,
                    pane_id,
                }) = self.state.rename_target.clone()
                {
                    let Some(ws_idx) = self
                        .state
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.id == workspace_id)
                    else {
                        cancel_rename_modal(&mut self.state);
                        return;
                    };
                    if let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) {
                        self.runtime_pane_rename(
                            "tui.pane.rename",
                            crate::api::schema::PaneRenameParams {
                                pane_id,
                                label: Some(new_name),
                            },
                        );
                    }
                }
            }
            _ => {}
        }

        self.state.set_server_mode(server_mode);
        cancel_rename_modal(&mut self.state);
    }

    fn save_snooze_time_modal_via_api(&mut self) {
        let Some(snooze) = self.state.sidebar_snooze.clone() else {
            return;
        };
        let Some(draft) = snooze.time_draft.as_deref() else {
            return;
        };
        let (hour, minute) = match parse_snooze_clock_time(draft) {
            Ok(time) => time,
            Err(message) => {
                if let Some(snooze) = self.state.sidebar_snooze.as_mut() {
                    snooze.error = Some(message.to_string());
                }
                return;
            }
        };
        let Some(deadline) = crate::platform::local_time_today_unix(hour, minute) else {
            if let Some(snooze) = self.state.sidebar_snooze.as_mut() {
                snooze.error = Some("Could not convert that local time".to_string());
            }
            return;
        };
        let now = crate::app::settled::unix_seconds(std::time::SystemTime::now());
        if let Err(message) = validate_snooze_deadline(now, deadline) {
            if let Some(snooze) = self.state.sidebar_snooze.as_mut() {
                snooze.error = Some(message.to_string());
            }
            return;
        }
        self.dispatch_snooze_time_deadline(snooze, deadline);
    }

    fn dispatch_snooze_time_deadline(
        &mut self,
        snooze: crate::app::state::SidebarSnoozeUiState,
        deadline: u64,
    ) {
        let Some(pane_id) = self.sidebar_pane_lifecycle_public_id(&snooze.target) else {
            self.state.sidebar_snooze = None;
            return;
        };
        self.dispatch_sidebar_pane_snooze(
            snooze.target,
            crate::api::schema::PaneSnoozeParams {
                pane_id,
                duration_s: None,
                snoozed_until: Some(deadline),
            },
        );
        self.state.sidebar_snooze = None;
    }

    pub(super) fn apply_rename_mouse_action_via_api(&mut self, action: ModalAction) {
        if self
            .state
            .sidebar_snooze
            .as_ref()
            .is_some_and(|snooze| snooze.time_draft.is_some())
        {
            match action {
                ModalAction::Save => self.save_snooze_time_modal_via_api(),
                ModalAction::Clear => {
                    if let Some(snooze) = self.state.sidebar_snooze.as_mut() {
                        if let Some(draft) = snooze.time_draft.as_mut() {
                            draft.clear();
                        }
                        snooze.error = None;
                    }
                }
                ModalAction::Cancel => self.state.sidebar_snooze = None,
                _ => {}
            }
            return;
        }
        match action {
            ModalAction::Save => self.save_rename_modal_via_api(),
            ModalAction::Clear => {
                self.state.name_input.clear();
                self.state.name_input_replace_on_type = false;
            }
            ModalAction::Cancel => cancel_rename_modal(&mut self.state),
            _ => {}
        }
    }

    pub(super) fn confirm_close_accept_via_api(&mut self) {
        if let Some(agent_ref) = self.state.confirm_close_remote_agent_ref.take() {
            if let Err(error) = self.remote_pane_close(agent_ref.clone()) {
                self.show_remote_pane_lifecycle_error(&agent_ref, error);
            }
        } else if let Some(ws_idx) = self.state.take_confirmed_workspace_close_index() {
            self.close_workspace_idx_with_group_via_api(ws_idx);
        }
        self.state.close_client_overlay();
    }

    pub(crate) fn handle_resize_key_via_api(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        if key.code == KeyCode::Esc
            || key.code == KeyCode::Enter
            || self.state.keybinds.resize_mode.matches_prefix_key(&raw_key)
            || self.state.keybinds.resize_mode.matches_direct_key(&raw_key)
        {
            self.state.set_server_mode(if self.state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            });
            return;
        }

        let direction = match key.code {
            KeyCode::Char('h') | KeyCode::Left => Some(NavDirection::Left),
            KeyCode::Char('l') | KeyCode::Right => Some(NavDirection::Right),
            KeyCode::Char('j') | KeyCode::Down => Some(NavDirection::Down),
            KeyCode::Char('k') | KeyCode::Up => Some(NavDirection::Up),
            _ => None,
        };
        if let Some(direction) = direction {
            self.runtime_pane_resize(
                "tui.pane.resize",
                crate::api::schema::PaneResizeParams {
                    pane_id: None,
                    direction: super::navigate::api_pane_direction(direction),
                    amount: None,
                },
            );
        }
    }

    pub(crate) fn handle_confirm_close_key_via_api(&mut self, key: KeyEvent) {
        match modal_action_from_key(&key, CONFIRM_CLOSE_ACTIONS) {
            Some(ModalAction::Confirm) => {
                self.confirm_close_accept_via_api();
            }
            Some(ModalAction::Cancel) => confirm_close_cancel(&mut self.state),
            _ => {}
        }
    }

    pub(crate) fn handle_context_menu_key_via_api(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state.context_menu = None;
                leave_modal(&mut self.state);
            }
            KeyCode::Up => {
                if let Some(menu) = self.state.context_menu.as_ref() {
                    let actions = self.state.context_menu_actions(menu);
                    let selected = actions
                        .iter()
                        .position(|action| *action == menu.selected)
                        .unwrap_or(0)
                        .saturating_sub(1);
                    if let (Some(menu), Some(action)) = (
                        self.state.context_menu.as_mut(),
                        actions.get(selected).copied(),
                    ) {
                        menu.selected = action;
                    }
                }
            }
            KeyCode::Down => {
                if let Some(menu) = self.state.context_menu.as_ref() {
                    let actions = self.state.context_menu_actions(menu);
                    let selected = actions
                        .iter()
                        .position(|action| *action == menu.selected)
                        .unwrap_or(0)
                        .saturating_add(1)
                        .min(actions.len().saturating_sub(1));
                    if let (Some(menu), Some(action)) = (
                        self.state.context_menu.as_mut(),
                        actions.get(selected).copied(),
                    ) {
                        menu.selected = action;
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(menu) = self.state.context_menu.take() {
                    let action = menu.selected;
                    self.apply_context_menu_action_via_api(menu, action);
                }
            }
            _ => {}
        }
    }

    /// Confirm a window-wide work link with a toast.
    ///
    /// A link click changes sidebar grouping rather than anything inside the
    /// pane, so without this the only feedback is a row moving somewhere the
    /// human may not be looking.
    fn show_work_linked_toast(
        &mut self,
        action: &crate::app::state::PaneMenuWorkLinkAction,
        ws_idx: usize,
        panes: usize,
    ) {
        let workspace_label = self
            .state
            .workspaces
            .get(ws_idx)
            .map(|ws| ws.display_name_from(&self.state.terminals, &self.terminal_runtimes))
            .unwrap_or_else(|| "workspace".into());
        let previous_toast = self.state.toast.clone();
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::WorkLinked,
            title: action.toast_title(),
            context: format!(
                "{workspace_label} · {panes} pane{}",
                if panes == 1 { "" } else { "s" }
            ),
            position: None,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
    }

    pub(crate) fn apply_context_menu_action_via_api(
        &mut self,
        mut menu: ContextMenuState,
        action: ContextMenuAction,
    ) {
        if !self.state.rebase_context_menu_indices(&mut menu) {
            self.state.close_client_overlay();
            return;
        }
        let actions = self.state.context_menu_actions(&menu);
        let item = actions
            .iter()
            .position(|candidate| *candidate == action)
            .and_then(|idx| self.state.context_menu_items(&menu).get(idx).copied());
        let (menu_x, menu_y) = (menu.x, menu.y);
        match (menu.kind, item) {
            (
                ContextMenuKind::RemoteAgent { agent_ref },
                Some(crate::app::state::UNSETTLE_ITEM),
            ) => {
                self.unsettle_sidebar_pane(crate::app::state::SidebarPaneLifecycleTarget::Remote(
                    agent_ref,
                ));
                self.state.close_client_overlay();
            }
            (ContextMenuKind::GitWorkspace { ws_idx, .. }, Some("New worktree")) => {
                self.state.request_new_linked_worktree = Some(ws_idx);
                self.state.close_client_overlay();
            }
            (ContextMenuKind::GitWorkspace { ws_idx, .. }, Some("Delete worktree checkout...")) => {
                self.state.request_remove_linked_worktree = Some(ws_idx);
                self.state.close_client_overlay();
            }
            (ContextMenuKind::GitWorkspace { ws_idx, .. }, Some("Open worktree...")) => {
                self.state.request_open_existing_worktree = Some(ws_idx);
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::GitWorkspace {
                    ws_idx, collapsed, ..
                },
                Some("Collapse" | "Expand"),
            ) => {
                if let Some((key, _)) = crate::ui::workspace_parent_group_state(&self.state, ws_idx)
                {
                    if collapsed {
                        self.state.collapsed_space_keys.remove(&key);
                    } else {
                        self.state.collapsed_space_keys.insert(key);
                    }
                    self.state.mark_session_dirty();
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Workspace { ws_idx, .. }
                | ContextMenuKind::GitWorkspace { ws_idx, .. },
                Some("Rename"),
            ) => open_rename_workspace(&mut self.state, &self.terminal_runtimes, ws_idx),
            (
                ContextMenuKind::Workspace { ws_idx, .. }
                | ContextMenuKind::GitWorkspace { ws_idx, .. },
                Some("Close" | "Close group"),
            ) => {
                self.state.selected = ws_idx;
                if self.state.confirm_close {
                    open_confirm_close(&mut self.state);
                } else {
                    self.close_workspace_idx_with_group_via_api(ws_idx);
                    self.state.close_client_overlay();
                }
            }
            (
                ContextMenuKind::Tab {
                    ws_idx, tab_idx, ..
                },
                Some("New tab"),
            ) => {
                self.focus_workspace_idx_via_api(ws_idx);
                self.focus_tab_idx_via_api(tab_idx);
                open_new_tab_dialog(&mut self.state);
            }
            (
                ContextMenuKind::Tab {
                    ws_idx, tab_idx, ..
                },
                Some("Rename"),
            ) => {
                self.focus_workspace_idx_via_api(ws_idx);
                self.focus_tab_idx_via_api(tab_idx);
                open_rename_active_tab(&mut self.state, false);
            }
            (
                ContextMenuKind::Tab {
                    ws_idx, tab_idx, ..
                },
                Some(crate::app::state::STAR_ITEM | crate::app::state::UNSTAR_ITEM),
            ) => {
                self.toggle_tab_star_via_api(ws_idx, tab_idx);
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Tab {
                    ws_idx, tab_idx, ..
                },
                Some(crate::app::state::MOVE_TO_SUBGROUP_ITEM),
            ) => {
                // The picker hangs off the menu cell the operator just chose,
                // the way every other downward menu hangs off its anchor.
                self.state.sidebar_subgroup_picker =
                    Some(crate::app::state::SidebarSubgroupPickerState {
                        ws_idx,
                        tab_idx,
                        anchor: (menu_x, menu_y),
                        filter: crate::ui::dropdown::DropdownFilterState::default(),
                    });
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Tab {
                    ws_idx, tab_idx, ..
                },
                Some(crate::app::state::REMOVE_FROM_SUBGROUP_ITEM),
            ) => {
                self.state.clear_tab_subgroup(ws_idx, tab_idx);
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Tab {
                    ws_idx,
                    snooze_target: Some(target),
                    ..
                },
                Some(crate::app::state::SNOOZE_ITEM),
            ) => {
                self.state.close_client_overlay();
                if let Some(target) = self.local_sidebar_pane_lifecycle_target(ws_idx, target) {
                    self.open_sidebar_snooze_menu(target, menu_x, menu_y);
                }
            }
            (
                ContextMenuKind::Tab {
                    ws_idx,
                    snooze_target: Some(target),
                    ..
                },
                Some(crate::app::state::SET_TIME_ITEM | crate::app::state::CHANGE_TIME_ITEM),
            ) => {
                self.state.close_client_overlay();
                if let Some(target) = self.local_sidebar_pane_lifecycle_target(ws_idx, target) {
                    self.open_snooze_time_input(target);
                }
            }
            (
                ContextMenuKind::Tab {
                    ws_idx,
                    snooze_target: Some(target),
                    ..
                },
                Some(crate::app::state::UNSNOOZE_ITEM),
            ) => {
                if let Some(pane_id) = self.public_pane_id(ws_idx, target) {
                    self.runtime_pane_unsnooze("tui.context-menu.unsnooze", pane_id);
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Tab {
                    workspace_id,
                    settle_pane_id: Some(pane_id),
                    ..
                },
                Some(crate::app::state::SETTLE_ITEM),
            ) => {
                self.settle_local_pane_or_tab(
                    crate::app::state::PaneFocusTarget {
                        workspace_id,
                        pane_id,
                    },
                    "tui.context-menu.settle",
                );
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Tab {
                    ws_idx,
                    settle_pane_id: Some(pane_id),
                    ..
                },
                Some(crate::app::state::UNSETTLE_ITEM),
            ) => {
                if let Some(target) = self.local_sidebar_pane_lifecycle_target(ws_idx, pane_id) {
                    self.unsettle_sidebar_pane(target);
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Tab {
                    ws_idx, tab_idx, ..
                },
                Some("Close"),
            ) => {
                self.close_tab_at_via_api(ws_idx, tab_idx);
                if self.state.client_overlay != ClientOverlay::ConfirmClose {
                    self.state.close_client_overlay();
                }
            }
            (
                ContextMenuKind::Pane {
                    workspace_id,
                    pane_id,
                    ..
                },
                Some("Rename pane"),
            ) => {
                open_rename_pane_in_workspace(&mut self.state, &workspace_id, pane_id);
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some(crate::app::state::SNOOZE_ITEM),
            ) => {
                self.state.close_client_overlay();
                if let Some(target) = self.local_sidebar_pane_lifecycle_target(ws_idx, pane_id) {
                    self.open_sidebar_snooze_menu(target, menu_x, menu_y);
                }
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some(crate::app::state::SET_TIME_ITEM | crate::app::state::CHANGE_TIME_ITEM),
            ) => {
                self.state.close_client_overlay();
                if let Some(target) = self.local_sidebar_pane_lifecycle_target(ws_idx, pane_id) {
                    self.open_snooze_time_input(target);
                }
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some(crate::app::state::UNSNOOZE_ITEM),
            ) => {
                if let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) {
                    self.runtime_pane_unsnooze("tui.context-menu.unsnooze", pane_id);
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx,
                    tab_idx,
                    linkable_work_link: Some(action),
                    ..
                },
                Some(item),
            ) if item == action.menu_item() => {
                let patch = action.patch();
                let mut linked = 0usize;
                for pane_id in self.state.window_pane_ids(ws_idx, tab_idx) {
                    if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                        self.runtime_pane_work_context_set(
                            "tui.pane.work_context.link_work_item",
                            crate::api::schema::PaneWorkContextSetParams {
                                pane_id: public_pane_id,
                                patch: patch.clone(),
                            },
                        );
                        linked += 1;
                    }
                }
                if linked > 0 {
                    self.show_work_linked_toast(&action, ws_idx, linked);
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    link: Some(link), ..
                },
                Some(crate::app::state::COPY_LINK_ITEM),
            ) => {
                self.state.request_clipboard_write = Some(link.into_bytes());
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    link: Some(link), ..
                },
                Some(crate::app::state::OPEN_LINK_ITEM),
            ) => {
                self.open_pane_link(link);
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx,
                    send_text: Some(text),
                    ..
                },
                Some(crate::app::state::SEND_TO_NEW_AGENT_ITEM),
            ) => {
                self.state.close_client_overlay();
                if let Err(error) = self.send_text_to_new_agent(ws_idx, &text) {
                    self.show_work_link_notice(&error);
                }
            }
            (
                ContextMenuKind::Pane {
                    ws_idx,
                    pane_id,
                    send_text: Some(text),
                    ..
                },
                Some(crate::app::state::SEND_TO_EXISTING_AGENT_ITEM),
            ) => {
                self.state.close_client_overlay();
                self.send_text_to_chosen_agent(ws_idx, pane_id, text);
            }
            (
                ContextMenuKind::Pane {
                    path: Some(path),
                    open_with,
                    ..
                },
                Some(item),
            ) if open_with
                .iter()
                .any(|target| item == target.file_item() || item == target.directory_item()) =>
            {
                let Some((target, directory)) = open_with.iter().find_map(|target| {
                    if item == target.file_item() {
                        Some((*target, false))
                    } else if item == target.directory_item() {
                        Some((*target, true))
                    } else {
                        None
                    }
                }) else {
                    return;
                };
                self.open_pane_path_with(target, &path, directory);
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some("Clear pane name"),
            ) => {
                if let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) {
                    self.runtime_pane_rename(
                        "tui.pane.clear_name",
                        crate::api::schema::PaneRenameParams {
                            pane_id,
                            label: None,
                        },
                    );
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some(action @ ("Send right-clicks to pane" | "Use Herdr right-click menu")),
            ) => {
                if let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) {
                    self.runtime_pane_input_set(
                        "tui.pane.input.set",
                        crate::api::schema::PaneInputSetParams {
                            pane_id,
                            right_click: if action == "Send right-clicks to pane" {
                                crate::api::schema::PaneRightClickTarget::Pane
                            } else {
                                crate::api::schema::PaneRightClickTarget::Herdr
                            },
                        },
                    );
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx,
                    pane_id,
                    source_pane_id: Some(source_pane_id),
                    ..
                },
                Some("Swap with focused pane"),
            ) => {
                let source_public_id = self.public_pane_id(ws_idx, source_pane_id);
                let target_public_id = self.public_pane_id(ws_idx, pane_id);
                if let (Some(source_public_id), Some(target_public_id)) =
                    (source_public_id, target_public_id)
                {
                    self.runtime_pane_swap(
                        "tui.pane.swap_exact",
                        crate::api::schema::PaneSwapParams {
                            pane_id: None,
                            direction: None,
                            source_pane_id: Some(source_public_id),
                            target_pane_id: Some(target_public_id),
                        },
                    );
                    self.focus_pane_internal_via_api(ws_idx, source_pane_id);
                }
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some("Split right"),
            ) => {
                self.focus_pane_internal_via_api(ws_idx, pane_id);
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Right);
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some("Split down"),
            ) => {
                self.focus_pane_internal_via_api(ws_idx, pane_id);
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Down);
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some("Zoom"),
            ) => {
                self.focus_pane_internal_via_api(ws_idx, pane_id);
                self.zoom_focused_pane_via_api();
                self.state.close_client_overlay();
            }
            (
                ContextMenuKind::Pane {
                    ws_idx, pane_id, ..
                },
                Some("Close pane"),
            ) => {
                self.focus_pane_internal_via_api(ws_idx, pane_id);
                if !self.close_focused_pane_via_api_requires_confirmation() {
                    self.state.close_client_overlay();
                }
            }
            _ => self.state.close_client_overlay(),
        }
    }
}

fn cancel_rename_modal(state: &mut AppState) {
    state.creating_new_tab = false;
    state.requested_new_tab_name = None;
    state.pending_workspace_create_cwd = None;
    state.rename_pane_target = None;
    state.rename_target = None;
    state.rename_tab_prefill = None;
    state.name_input.clear();
    state.name_input_replace_on_type = false;
    leave_modal(state);
}

fn parse_snooze_clock_time(input: &str) -> Result<(u8, u8), &'static str> {
    let Some((hour, minute)) = input.trim().split_once(':') else {
        return Err("Use a 24-hour time such as 14:30");
    };
    if hour.is_empty()
        || minute.len() != 2
        || !hour.bytes().all(|byte| byte.is_ascii_digit())
        || !minute.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Use a 24-hour time such as 14:30");
    }
    let hour = hour
        .parse::<u8>()
        .map_err(|_| "Use a 24-hour time such as 14:30")?;
    let minute = minute
        .parse::<u8>()
        .map_err(|_| "Use a 24-hour time such as 14:30")?;
    if hour > 23 || minute > 59 {
        return Err("Use a time between 00:00 and 23:59");
    }
    Ok((hour, minute))
}

fn validate_snooze_deadline(now: u64, deadline: u64) -> Result<u64, &'static str> {
    if deadline <= now {
        Err("Time must be later than now")
    } else {
        Ok(deadline)
    }
}

impl AppState {
    pub(super) fn global_menu_item_at(&self, col: u16, row: u16) -> Option<GlobalMenuAction> {
        let rect = self.global_menu_rect();
        if col <= rect.x
            || col >= rect.x + rect.width.saturating_sub(1)
            || row <= rect.y
            || row >= rect.y + rect.height.saturating_sub(1)
        {
            return None;
        }
        let idx = (row - rect.y - 1) as usize;
        global_menu_actions(self).get(idx).copied()
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::{Direction, Rect};

    use super::super::{capture_snapshot, state_with_workspaces};
    use super::*;
    use crate::workspace::Workspace;

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-modal-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("config.toml")
    }

    fn app_with_test_workspaces(names: &[&str]) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        app.state.ensure_test_terminals();
        app.state.active = (!app.state.workspaces.is_empty()).then_some(0);
        app.state.selected = 0;
        app.state.set_server_mode(if app.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        });
        app
    }

    fn context_tab_ids(state: &AppState, ws_idx: usize, tab_idx: usize) -> (String, String) {
        let workspace_id = state.workspaces[ws_idx].id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace_id,
            state.workspaces[ws_idx].tabs[tab_idx].number,
        );
        (workspace_id, tab_id)
    }

    fn pane_context_menu(
        state: &AppState,
        pane_id: crate::layout::PaneId,
        selected: ContextMenuAction,
    ) -> ContextMenuState {
        let (workspace_id, tab_id) = context_tab_ids(state, 0, 0);
        ContextMenuState {
            kind: ContextMenuKind::Pane {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                pane_id,
                source_pane_id: None,
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
                link: None,
                path: None,
                open_with: Vec::new(),
                send_text: None,
                has_agent_targets: false,
            },
            x: 4,
            y: 2,
            selected,
        }
    }

    /// The Repo view groups a bound Space with the checkout Spaces of its
    /// repository, and those Spaces carry no worktree membership. Collapse has to
    /// write the key the sidebar reads back, or the menu item does nothing.
    #[test]
    fn collapsing_a_repository_group_writes_the_key_the_sidebar_reads() {
        let mut app = app_with_test_workspaces(&["scalablev2", "checkout"]);
        app.state.workspaces[0].repo_binding = Some("scalable-so/scalablev2".into());
        let pane_id = app.state.workspaces[1].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[1].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("checkout terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: Some("scalable-so/scalablev2".into()),
                ..Default::default()
            })
            .expect("work context");

        let (key, collapsed) = crate::ui::workspace_parent_group_state(&app.state, 0)
            .expect("the bound Space heads the group");
        assert_eq!(key, "repo:scalable-so/scalablev2");
        assert!(!collapsed);

        let mut menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                workspace_id: app.state.workspaces[0].id.clone(),
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::RenameWorkspace,
        };
        let collapse_idx = menu
            .items()
            .iter()
            .position(|item| *item == "Collapse")
            .expect("collapse item");
        assert_eq!(menu.items()[collapse_idx], "Collapse");
        menu.selected = ContextMenuAction::CollapseWorkspace;
        app.state.context_menu = Some(menu);

        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(app.state.collapsed_space_keys.contains(&key));
        assert_eq!(
            crate::ui::workspace_parent_group_state(&app.state, 0),
            Some((key, true))
        );
    }

    #[test]
    fn git_menu_info_row_ignores_enter_and_escape_closes_it() {
        let mut app = app_with_test_workspaces(&["outside"]);
        let workspace = &app.state.workspaces[0];
        let pane_id = workspace.focused_pane_id().expect("focused pane");
        let terminal_id = workspace.terminal_id(pane_id).expect("terminal").clone();
        let cwd = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal state")
            .cwd
            .clone();
        app.state.git_root_for_cwd.insert(cwd, None);
        app.state.set_server_mode(Mode::GitMenu);

        handle_git_menu_key(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(app.state.request_git_action, None);
        assert_eq!(app.state.server_mode(), Mode::GitMenu);

        handle_git_menu_key(
            &mut app.state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn workspace_create_label_preserves_auto_name_for_suggestion_or_blank() {
        assert_eq!(workspace_create_label("project", "project"), None);
        assert_eq!(workspace_create_label("", "project"), None);
        assert_eq!(workspace_create_label("   ", "project"), None);
        assert_eq!(
            workspace_create_label("  logs  ", "project").as_deref(),
            Some("logs")
        );
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

    #[test]
    fn custom_resize_key_exits_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.set_server_mode(Mode::Resize);
        state.keybinds.resize_mode = crate::config::ActionKeybinds::prefix("g");

        handle_resize_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn direct_resize_key_exits_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.set_server_mode(Mode::Resize);
        state.keybinds.resize_mode = crate::config::ActionKeybinds::direct("ctrl+alt+r");

        handle_resize_key(
            &mut state,
            TerminalKey::new(
                KeyCode::Char('r'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn resize_key_exit_matches_enhanced_shifted_punctuation() {
        let mut state = state_with_workspaces(&["test"]);
        state.set_server_mode(Mode::Resize);
        state.keybinds.resize_mode = crate::config::ActionKeybinds::prefix("?");

        handle_resize_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn detach_requests_client_detach_in_persistence_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.detach_exits = false;

        request_detach(&mut state);

        assert!(state.detach_requested);
        assert!(!state.should_quit);
    }

    #[test]
    fn detach_exits_in_no_session_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.detach_exits = true;

        request_detach(&mut state);

        assert!(state.should_quit);
        assert!(!state.detach_requested);
    }

    #[test]
    fn global_menu_whats_new_opens_saved_release_notes() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let path = temp_config_path("whats-new-saved-release-notes");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
        crate::release_notes::save_pending(env!("CARGO_PKG_VERSION"), "### Changed\n- Menu")
            .unwrap();

        let mut state = state_with_workspaces(&["test"]);
        state.latest_release_notes_available = true;

        assert!(global_menu_actions(&state).contains(&GlobalMenuAction::WhatsNew));

        apply_global_menu_action(&mut state, GlobalMenuAction::WhatsNew);

        assert_eq!(state.server_mode(), Mode::ReleaseNotes);
        assert_eq!(
            state
                .release_notes
                .as_ref()
                .map(|notes| notes.body.as_str()),
            Some("### Changed\n- Menu")
        );

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rename_modal_keyboard_and_mouse_share_actions() {
        let mut state = state_with_workspaces(&["test"]);
        state.set_server_mode(Mode::Terminal);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
        state.name_input = "hello".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(state.name_input.is_empty());

        state.name_input = "renamed".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(state.server_mode(), Mode::Terminal);
        assert_eq!(state.workspaces[0].display_name(), "renamed");
        let snapshot = capture_snapshot(&state);
        assert_eq!(
            snapshot.workspaces[0].custom_name.as_deref(),
            Some("renamed")
        );

        state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        state.view.terminal_area = Rect::new(26, 0, 80, 20);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
        state.name_input = "mouse".into();
        let inner = state.rename_modal_inner().unwrap();
        let (save, _, _) = crate::ui::rename_button_rects(inner);
        let action = modal_action_from_buttons(save.x, save.y, &[(save, ModalAction::Save)]);
        assert_eq!(action, Some(ModalAction::Save));
    }

    #[test]
    fn tab_rename_updates_captured_snapshot() {
        let mut state = state_with_workspaces(&["test"]);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        state.name_input = "logs".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        let snapshot = capture_snapshot(&state);
        assert_eq!(
            snapshot.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("logs")
        );
    }

    #[test]
    fn rename_cancel_returns_to_terminal_when_workspace_is_active() {
        let mut state = state_with_workspaces(&["test"]);
        state.set_server_mode(Mode::Terminal);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        state.name_input = "test".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);
        assert!(state.name_input.is_empty());
    }

    #[test]
    fn rename_modal_replaces_prefilled_text_on_first_type() {
        let mut state = state_with_workspaces(&["test"]);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        state.name_input = "2".into();
        state.name_input_replace_on_type = true;

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()),
        );
        assert_eq!(state.name_input, "n");
        assert!(!state.name_input_replace_on_type);

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()),
        );
        assert_eq!(state.name_input, "ne");
    }

    #[test]
    fn rename_modal_replaces_prefilled_text_on_paste() {
        let mut state = state_with_workspaces(&["test"]);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        state.name_input = "2".into();
        state.name_input_replace_on_type = true;

        insert_rename_input_text(&mut state, "feature/logs");

        assert_eq!(state.name_input, "feature/logs");
        assert!(!state.name_input_replace_on_type);

        insert_rename_input_text(&mut state, "-copy");

        assert_eq!(state.name_input, "feature/logs-copy");
    }

    #[test]
    fn rename_modal_handles_line_editing_shortcuts() {
        let mut state = state_with_workspaces(&["test"]);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
        state.name_input = "website zero".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()),
        );
        assert_eq!(state.name_input, "website zer");

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website ");

        state.name_input = "website-zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
        );
        assert_eq!(state.name_input, "website-");

        state.name_input = "website-zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website-");

        state.name_input = "website-zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website-");

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER),
        );
        assert!(state.name_input.is_empty());

        state.name_input = "website zero".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert!(state.name_input.is_empty());
    }

    #[test]
    fn rename_modal_does_not_insert_modified_shortcut_chars() {
        let mut state = state_with_workspaces(&["test"]);
        state.open_client_overlay(crate::app::state::ClientOverlay::RenameWorkspace);
        state.name_input = "website".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.name_input, "website");

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.name_input, "websiteZ");
    }

    #[test]
    fn keybind_help_slash_focuses_filter_and_preserves_vim_scroll() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybind_help.query = "stale".into();
        state.keybind_help.search_focused = true;
        state.view.terminal_area = Rect::new(0, 0, 100, 30);

        open_keybind_help(&mut state);
        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );
        assert_eq!(state.keybind_help.scroll, 1);
        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()),
        );
        assert_eq!(state.keybind_help.scroll, 0);

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('w'), KeyModifiers::empty()),
        );
        assert!(state.keybind_help.query.is_empty());

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::empty()),
        );
        for character in "work".chars() {
            state.keybind_help.scroll = 2;
            handle_keybind_help_key(
                &mut state,
                TerminalKey::new(KeyCode::Char(character), KeyModifiers::empty()),
            );
        }

        assert!(state.keybind_help.search_focused);
        assert_eq!(state.keybind_help.query, "work");
        assert_eq!(state.keybind_help.scroll, 0);
    }

    #[test]
    fn keybind_help_query_supports_backspace_clear_and_sanitized_paste() {
        let mut state = state_with_workspaces(&["test"]);
        open_keybind_help(&mut state);
        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::empty()),
        );

        insert_keybind_help_query_text(&mut state, "work\nspace");
        assert_eq!(state.keybind_help.query, "workspace");

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Backspace, KeyModifiers::empty()),
        );
        assert_eq!(state.keybind_help.query, "workspac");

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert!(state.keybind_help.query.is_empty());
    }

    #[test]
    fn keybind_help_escape_leaves_search_before_closing() {
        let mut state = state_with_workspaces(&["test"]);
        open_keybind_help(&mut state);
        state.keybind_help.search_focused = true;
        state.keybind_help.query = "work".into();

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()),
        );
        assert_eq!(state.server_mode(), Mode::KeybindHelp);
        assert!(!state.keybind_help.search_focused);
        assert!(state.keybind_help.query.is_empty());

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()),
        );
        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn enhanced_shifted_slash_focuses_keybind_help_filter() {
        let mut state = state_with_workspaces(&["test"]);
        open_keybind_help(&mut state);

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('7'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('/' as u32),
        );

        assert!(state.keybind_help.search_focused);
    }

    #[test]
    fn enhanced_shifted_question_mark_closes_keybind_help_when_not_searching() {
        let mut state = state_with_workspaces(&["test"]);
        open_keybind_help(&mut state);

        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);

        open_keybind_help(&mut state);
        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::empty()),
        );
        handle_keybind_help_key(
            &mut state,
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        );

        assert_eq!(state.keybind_help.query, "?");
    }

    #[test]
    fn navigator_search_accepts_pasted_text_when_focused() {
        let mut state = state_with_workspaces(&["alpha", "beta"]);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Navigator);
        state.navigator.search_focused = true;
        state.navigator.state_filter = Some(NavigatorStateFilter::Working);

        insert_navigator_search_text(&mut state, &terminal_runtimes, "beta");

        assert_eq!(state.navigator.query, "beta");
        assert_eq!(state.navigator.state_filter, None);
    }

    #[test]
    fn navigator_search_ignores_paste_when_search_is_not_focused() {
        let mut state = state_with_workspaces(&["alpha", "beta"]);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Navigator);
        state.navigator.search_focused = false;

        insert_navigator_search_text(&mut state, &terminal_runtimes, "beta");

        assert!(state.navigator.query.is_empty());
    }

    #[test]
    fn navigator_empty_search_escape_returns_to_commands() {
        let mut state = state_with_workspaces(&["alpha", "beta"]);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Navigator);
        state.navigator.search_focused = true;

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Navigator);
        assert!(!state.navigator.search_focused);
        assert!(state.navigator.query.is_empty());

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::empty()),
        );

        assert_eq!(
            state.navigator.state_filter,
            Some(NavigatorStateFilter::Working)
        );
        assert!(state.navigator.query.is_empty());

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn navigator_search_escape_blurs_then_next_escape_closes() {
        let mut state = state_with_workspaces(&["alpha", "beta"]);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Navigator);
        state.navigator.search_focused = true;
        state.navigator.query = "a".into();

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Navigator);
        assert!(!state.navigator.search_focused);
        assert_eq!(state.navigator.query, "a");

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );

        assert_eq!(state.navigator.selected, 1);
        assert_eq!(state.navigator.query, "a");

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Navigator);
        assert!(state.navigator.search_focused);
        assert_eq!(state.navigator.query, "a");

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::empty()),
        );

        assert_eq!(state.navigator.query, "al");

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Navigator);
        assert!(!state.navigator.search_focused);

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn navigator_ignores_modified_j_and_k() {
        let mut state = state_with_workspaces(&["alpha", "beta"]);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        state.set_server_mode(Mode::Navigator);
        state.navigator.selected = 1;

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        );

        assert_eq!(state.navigator.selected, 1);

        handle_navigator_key(
            &mut state,
            &terminal_runtimes,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );

        assert_eq!(state.navigator.selected, 1);
    }

    #[test]
    fn open_rename_active_tab_can_prefill_default_new_tab_name() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_add_tab(None);
        state.workspaces[0].switch_tab(1);

        open_rename_active_tab(&mut state, true);

        assert_eq!(state.effective_interaction_mode(), Mode::RenameTab);
        assert_eq!(state.name_input, "2");
        assert!(state.name_input_replace_on_type);
    }

    #[test]
    fn cancel_new_tab_dialog_leaves_workspace_unchanged() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.client_overlay, ClientOverlay::None);
        assert!(!state.creating_new_tab);
        assert!(!state.request_new_tab);
        assert!(state.requested_new_tab_name.is_none());
        assert_eq!(state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn saving_new_tab_dialog_requests_creation_with_name() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);
        state.name_input = "logs".into();
        state.name_input_replace_on_type = false;

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.client_overlay, ClientOverlay::None);
        assert!(!state.creating_new_tab);
        assert!(state.request_new_tab);
        assert_eq!(state.requested_new_tab_name.as_deref(), Some("logs"));
    }

    #[test]
    fn saving_new_tab_dialog_with_default_name_keeps_tab_auto_named() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.client_overlay, ClientOverlay::None);
        assert!(!state.creating_new_tab);
        assert!(state.request_new_tab);
        assert!(state.requested_new_tab_name.is_none());
    }

    #[test]
    fn closing_first_auto_tab_compacts_remaining_auto_tab_label_and_next_prompt() {
        let mut state = state_with_workspaces(&["test"]);
        open_new_tab_dialog(&mut state);
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        state.workspaces[0].test_add_tab(state.requested_new_tab_name.as_deref());
        state.request_new_tab = false;
        state.requested_new_tab_name = None;

        state.workspaces[0].close_tab(0);
        state.workspaces[0].switch_tab(0);

        assert_eq!(
            state.workspaces[0]
                .tab_display_name_from(&state.terminals, 0)
                .unwrap_or_else(|| "1".into()),
            "1"
        );
        assert!(state.workspaces[0].tabs[0].custom_name.is_none());

        open_new_tab_dialog(&mut state);
        assert_eq!(state.name_input, "2");
    }

    #[test]
    fn unedited_rename_enter_stays_a_no_op_when_the_live_label_changes() {
        let mut state = state_with_workspaces(&["test"]);
        state.ensure_test_terminals();
        let tab = &state.workspaces[0].tabs[0];
        let terminal_id = tab
            .terminal_id(tab.layout.focused())
            .cloned()
            .expect("focused terminal");
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .agent_name = Some("claude".into());

        open_rename_active_tab(&mut state, false);
        assert_eq!(state.name_input, "claude");

        // The agent advances while the modal is open; the user never touches the input.
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .agent_name = Some("codex".into());

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.client_overlay, ClientOverlay::None);
        assert!(
            state.workspaces[0].tabs[0].custom_name.is_none(),
            "an unedited Enter must not pin the stale prefill as a user name"
        );
        assert!(state.rename_tab_prefill.is_none());
    }

    #[test]
    fn renaming_auto_tab_to_its_default_number_keeps_it_auto_named() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_add_tab(None);
        state.workspaces[0].switch_tab(1);

        open_rename_active_tab(&mut state, false);
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.client_overlay, ClientOverlay::None);
        assert!(state.workspaces[0].tabs[1].custom_name.is_none());
        assert_eq!(
            state.workspaces[0]
                .tab_display_name_from(&state.terminals, 1)
                .unwrap_or_else(|| "2".into()),
            "2"
        );
    }

    #[test]
    fn confirm_close_keyboard_actions_are_direct_not_focused() {
        let mut state = state_with_workspaces(&["a", "b"]);
        state.selected = 1;
        open_confirm_close(&mut state);

        handle_confirm_close_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );
        assert_eq!(state.server_mode(), Mode::Navigate);
        assert_eq!(state.workspaces.len(), 2);

        open_confirm_close(&mut state);
        handle_confirm_close_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn confirm_close_for_linked_worktree_closes_workspace_only() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        state.selected = 1;
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        open_confirm_close(&mut state);
        handle_confirm_close_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "main");
        assert_eq!(state.client_overlay, ClientOverlay::None);
    }

    #[test]
    fn context_menu_close_group_opens_group_close_confirmation() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        state.active = Some(0);
        state.selected = 1;
        state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                workspace_id: state.workspaces[0].id.clone(),
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::CloseWorkspace,
        };
        let mut terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        apply_context_menu_action(
            &mut state,
            &mut terminal_runtimes,
            menu,
            ContextMenuAction::CloseWorkspace,
        );

        assert_eq!(state.selected, 0);
        assert_eq!(state.effective_interaction_mode(), Mode::ConfirmClose);

        confirm_close_accept(&mut state);

        assert!(state.workspaces.is_empty());
        assert_eq!(state.client_overlay, ClientOverlay::None);
    }

    #[test]
    fn context_menu_toggles_pane_right_click_passthrough() {
        let mut app = app_with_test_workspaces(&["main"]);
        app.state.active = Some(0);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                pane_id,
                source_pane_id: None,
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
                link: None,
                path: None,
                open_with: Vec::new(),
                send_text: None,
                has_agent_targets: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::SendRightClicksToPane,
        };
        app.apply_context_menu_action_via_api(menu, ContextMenuAction::SendRightClicksToPane);

        assert!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .right_click_passthrough
        );
    }

    #[test]
    fn context_menu_close_pane_last_parent_group_pane_confirms_group_close() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        state.active = Some(0);
        state.selected = 1;
        state.confirm_close = true;
        state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let (workspace_id, tab_id) = context_tab_ids(&state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                pane_id,
                source_pane_id: None,
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
                link: None,
                path: None,
                open_with: Vec::new(),
                send_text: None,
                has_agent_targets: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::ClosePane,
        };
        let mut terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        apply_context_menu_action(
            &mut state,
            &mut terminal_runtimes,
            menu,
            ContextMenuAction::ClosePane,
        );

        assert_eq!(state.selected, 0);
        assert_eq!(state.effective_interaction_mode(), Mode::ConfirmClose);
        assert_eq!(state.workspaces.len(), 2);
    }

    #[test]
    fn api_confirm_close_accept_closes_parent_worktree_group() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.selected = 0;
        open_confirm_close(&mut app.state);

        app.handle_confirm_close_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(app.state.workspaces.is_empty());
        assert_eq!(app.state.client_overlay, ClientOverlay::None);
        assert_eq!(app.event_hub.events_after(0).len(), 2);
    }

    #[test]
    fn api_confirm_close_accept_keeps_the_original_workspace_target() {
        let mut app = app_with_test_workspaces(&["main", "issue", "other"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.selected = 0;
        open_confirm_close(&mut app.state);

        app.focus_workspace_idx_via_api(2);
        assert_eq!(app.state.selected, 2);
        assert_eq!(app.state.effective_interaction_mode(), Mode::ConfirmClose);

        app.handle_confirm_close_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "other");
        assert_eq!(
            app.event_hub
                .events_after(0)
                .iter()
                .filter(|(_, event)| matches!(
                    event.event,
                    crate::api::schema::EventKind::WorkspaceClosed
                ))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn api_context_menu_close_tab_last_parent_group_workspace_confirms_group_close() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.confirm_close = true;
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::ContextMenu);
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: None,
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::CloseTab,
        };
        app.apply_context_menu_action_via_api(menu, ContextMenuAction::CloseTab);

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.effective_interaction_mode(), Mode::ConfirmClose);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(!app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[tokio::test]
    async fn api_context_menu_close_inactive_tab_does_not_confirm_for_single_tab_active_workspace()
    {
        let mut app = app_with_test_workspaces(&["target", "active"]);
        app.state.workspaces[0].test_add_tab(Some("second"));
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.confirm_close = true;
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::ContextMenu);
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: None,
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::CloseTab,
        };

        app.apply_context_menu_action_via_api(menu, ContextMenuAction::CloseTab);

        assert_eq!(app.state.effective_interaction_mode(), Mode::Terminal);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[1].tabs.len(), 1);
    }

    #[test]
    fn tab_context_menu_offers_subgroup_actions_exactly_when_relevant() {
        let state = state_with_workspaces(&["main"]);
        let (workspace_id, tab_id) = context_tab_ids(&state, 0, 0);
        let plain = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id: workspace_id.clone(),
                tab_id: tab_id.clone(),
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: None,
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::NewTab,
        };
        assert_eq!(
            plain.items(),
            vec![
                "New tab",
                "Rename",
                crate::app::state::STAR_ITEM,
                crate::app::state::MOVE_TO_SUBGROUP_ITEM,
                "Close",
            ],
            "a window with no subgroup gets the move action only"
        );
        let member = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: true,
                settle_pane_id: None,
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::NewTab,
        };
        assert_eq!(
            member.items(),
            vec![
                "New tab",
                "Rename",
                crate::app::state::STAR_ITEM,
                crate::app::state::MOVE_TO_SUBGROUP_ITEM,
                crate::app::state::REMOVE_FROM_SUBGROUP_ITEM,
                "Close",
            ],
            "a window in a subgroup also gets the remove action"
        );
    }

    #[test]
    fn api_sidebar_context_menu_settles_the_exact_pane() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let sibling_pane = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: Some(pane_id),
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::Settle,
        };
        app.apply_context_menu_action_via_api(menu, ContextMenuAction::Settle);

        assert!(app.state.pane_is_settled(0, pane_id));
        assert!(!app.state.pane_is_settled(0, sibling_pane));
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn api_sidebar_context_menu_unsettles_the_exact_pane() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        app.state.settle_pane_at(0, pane_id, 1_725_000_000);
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: Some(pane_id),
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::Unsettle,
        };
        assert!(app
            .state
            .context_menu_actions(&menu)
            .contains(&ContextMenuAction::Unsettle));
        app.apply_context_menu_action_via_api(menu, ContextMenuAction::Unsettle);
        assert!(!app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn sections_context_menu_settle_settles_every_pane_in_split_tab() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let sibling_pane = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.sidebar_sections_layout = true;
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: Some(pane_id),
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::Settle,
        };

        app.apply_context_menu_action_via_api(menu, ContextMenuAction::Settle);

        assert!(app.state.pane_is_settled(0, pane_id));
        assert!(app.state.pane_is_settled(0, sibling_pane));
    }

    #[test]
    fn api_context_menu_move_to_subgroup_opens_the_picker_at_the_menu() {
        let mut app = app_with_test_workspaces(&["main"]);
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: false,
                settle_pane_id: None,
                snooze_target: None,
            },
            x: 7,
            y: 4,
            selected: ContextMenuAction::MoveToSubgroup,
        };
        app.apply_context_menu_action_via_api(menu, ContextMenuAction::MoveToSubgroup);

        let picker = app
            .state
            .sidebar_subgroup_picker
            .as_ref()
            .expect("the subgroup picker opens");
        assert_eq!((picker.ws_idx, picker.tab_idx), (0, 0));
        assert_eq!(picker.anchor, (7, 4));
    }

    #[test]
    fn api_context_menu_remove_from_subgroup_clears_the_assignment() {
        let mut app = app_with_test_workspaces(&["main"]);
        app.state.workspaces[0].tabs[0].set_subgroup(Some("api".to_string()));
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                starred: false,
                has_subgroup: true,
                settle_pane_id: None,
                snooze_target: None,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::RemoveFromSubgroup,
        };
        app.apply_context_menu_action_via_api(menu, ContextMenuAction::RemoveFromSubgroup);

        assert_eq!(app.state.workspaces[0].tabs[0].subgroup(), None);
    }

    #[tokio::test]
    async fn api_context_menu_enter_close_pane_last_parent_group_pane_confirms_group_close() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.confirm_close = true;
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::ContextMenu);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let mut menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                pane_id,
                source_pane_id: None,
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
                link: None,
                path: None,
                open_with: Vec::new(),
                send_text: None,
                has_agent_targets: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::ClosePane,
        };
        assert!(menu.items().contains(&"Close pane"));
        menu.selected = ContextMenuAction::ClosePane;
        app.state.context_menu = Some(menu);

        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.effective_interaction_mode(), Mode::ConfirmClose);
        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(app.state.workspaces[0].pane_state(pane_id).is_some());
        assert!(app.state.context_menu.is_none());
    }

    #[test]
    fn snooze_clock_input_accepts_clock_time_and_rejects_past_deadlines() {
        assert_eq!(parse_snooze_clock_time("14:30"), Ok((14, 30)));
        assert_eq!(parse_snooze_clock_time("7:05"), Ok((7, 5)));
        assert_eq!(
            parse_snooze_clock_time("24:00"),
            Err("Use a time between 00:00 and 23:59")
        );
        assert_eq!(
            validate_snooze_deadline(1_000, 999),
            Err("Time must be later than now")
        );
    }

    #[test]
    fn context_menus_name_snooze_actions_for_both_states() {
        let pane_id = crate::layout::PaneId::alloc();
        for snoozed in [false, true] {
            let tab = ContextMenuState {
                kind: ContextMenuKind::Tab {
                    workspace_id: "workspace".into(),
                    tab_id: "workspace:t1".into(),
                    ws_idx: 0,
                    tab_idx: 0,
                    settle_pane_id: Some(pane_id),
                    snooze_target: Some(pane_id),
                    starred: false,
                    has_subgroup: false,
                },
                x: 0,
                y: 0,
                selected: ContextMenuAction::NewTab,
            };
            let items = tab.items_for_pane_state(snoozed, !snoozed, true);
            if snoozed {
                assert!(items.contains(&crate::app::state::UNSNOOZE_ITEM));
                assert!(items.contains(&crate::app::state::CHANGE_TIME_ITEM));
                assert!(!items.contains(&crate::app::state::SNOOZE_ITEM));
            } else {
                assert!(items.contains(&crate::app::state::SNOOZE_ITEM));
                assert!(items.contains(&crate::app::state::SET_TIME_ITEM));
                assert!(!items.contains(&crate::app::state::UNSNOOZE_ITEM));
            }

            let pane = ContextMenuState {
                kind: ContextMenuKind::Pane {
                    workspace_id: "workspace".into(),
                    tab_id: "workspace:t1".into(),
                    ws_idx: 0,
                    tab_idx: 0,
                    pane_id,
                    source_pane_id: None,
                    has_manual_label: false,
                    right_click_passthrough: false,
                    linkable_work_link: None,
                    link: None,
                    path: None,
                    open_with: Vec::new(),
                    send_text: None,
                    has_agent_targets: false,
                },
                x: 0,
                y: 0,
                selected: ContextMenuAction::RenamePane,
            };
            assert_eq!(
                pane.items_for_pane_state(snoozed, !snoozed, true)
                    .contains(&crate::app::state::UNSNOOZE_ITEM),
                snoozed
            );
        }
    }

    #[test]
    fn live_snooze_removes_settle_without_rebinding_enter_to_new_tab() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].test_add_tab(Some("keep"));
        app.state.ensure_test_terminals();
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 0, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                settle_pane_id: Some(pane_id),
                snooze_target: Some(pane_id),
                starred: false,
                has_subgroup: false,
            },
            x: 3,
            y: 2,
            selected: ContextMenuAction::Settle,
        };
        assert!(app
            .state
            .context_menu_items(&menu)
            .contains(&crate::app::state::SETTLE_ITEM));
        app.state.context_menu = Some(menu);
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::ContextMenu);

        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 900;
        assert!(app.state.snooze_pane_at(0, pane_id, deadline));
        let live_items = app
            .state
            .context_menu
            .as_ref()
            .map(|menu| app.state.context_menu_items(menu))
            .expect("open menu");
        assert!(!live_items.contains(&crate::app::state::SETTLE_ITEM));
        assert!(app.reconcile_client_interaction(false));
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 30));
        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
    }

    #[test]
    fn concurrent_snooze_does_not_rebind_dropdown_enter_to_unsnooze() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let target = app
            .local_sidebar_pane_lifecycle_target(0, pane_id)
            .expect("local lifecycle target");
        app.open_sidebar_snooze_menu(target, 3, 2);
        let selected = app
            .state
            .sidebar_snooze
            .as_ref()
            .expect("snooze dropdown")
            .selected;
        assert!(matches!(
            selected,
            crate::app::state::SidebarSnoozeMenuAction::Preset(_)
        ));

        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 900;
        assert!(app.state.snooze_pane_at(0, pane_id, deadline));
        app.handle_sidebar_snooze_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(app.state.pane_is_snoozed(0, pane_id));
        assert!(app.state.sidebar_snooze.is_none());
    }

    #[test]
    fn context_menu_settle_resolves_original_workspace_after_reorder() {
        let mut app = app_with_test_workspaces(&["first", "second"]);
        let pane_id = app.state.workspaces[1].tabs[0].root_pane;
        let (workspace_id, tab_id) = context_tab_ids(&app.state, 1, 0);
        let menu = ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id: workspace_id.clone(),
                tab_id,
                ws_idx: 1,
                tab_idx: 0,
                settle_pane_id: Some(pane_id),
                snooze_target: Some(pane_id),
                starred: false,
                has_subgroup: false,
            },
            x: 3,
            y: 2,
            selected: ContextMenuAction::Settle,
        };
        app.state.workspaces.swap(0, 1);

        app.apply_context_menu_action_via_api(menu, ContextMenuAction::Settle);

        let ws_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .expect("original workspace");
        assert!(app.state.pane_is_settled(ws_idx, pane_id));
    }

    #[test]
    fn rename_targets_survive_workspace_and_tab_reorder() {
        let mut state = state_with_workspaces(&["first", "second"]);
        let workspace_id = state.workspaces[1].id.clone();
        open_rename_workspace(
            &mut state,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            1,
        );
        state.name_input = "renamed workspace".into();
        state.workspaces.swap(0, 1);
        apply_rename_action(&mut state, ModalAction::Save);
        let workspace = state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .expect("original workspace");
        assert_eq!(workspace.custom_name.as_deref(), Some("renamed workspace"));

        state.active = Some(0);
        state.workspaces[0].test_add_tab(Some("other"));
        let tab_number = state.workspaces[0].tabs[0].number;
        open_rename_active_tab(&mut state, false);
        state.name_input = "renamed tab".into();
        assert!(state.workspaces[0].move_tab(0, 2));
        apply_rename_action(&mut state, ModalAction::Save);
        let tab = state.workspaces[0]
            .tabs
            .iter()
            .find(|tab| tab.number == tab_number)
            .expect("original tab");
        assert_eq!(tab.custom_name.as_deref(), Some("renamed tab"));
    }

    #[test]
    fn pane_rename_menu_uses_its_workspace_after_focus_changes() {
        let mut app = app_with_test_workspaces(&["first", "second"]);
        app.state.ensure_test_terminals();
        let workspace_id = app.state.workspaces[1].id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace_id,
            app.state.workspaces[1].tabs[0].number,
        );
        let pane_id = app.state.workspaces[1].tabs[0].root_pane;
        let menu = ContextMenuState {
            kind: ContextMenuKind::Pane {
                workspace_id: workspace_id.clone(),
                tab_id,
                ws_idx: 1,
                tab_idx: 0,
                pane_id,
                source_pane_id: None,
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
                link: None,
                path: None,
                open_with: Vec::new(),
                send_text: None,
                has_agent_targets: false,
            },
            x: 3,
            y: 2,
            selected: ContextMenuAction::RenamePane,
        };
        app.state.active = Some(0);
        app.state.selected = 0;

        app.apply_context_menu_action_via_api(menu, ContextMenuAction::RenamePane);

        assert_eq!(app.state.effective_interaction_mode(), Mode::RenamePane);
        assert_eq!(
            app.state.rename_target,
            Some(crate::app::state::RenameTarget::Pane {
                workspace_id,
                pane_id,
            })
        );
    }

    #[test]
    fn set_time_and_unsnooze_context_actions_dispatch_through_the_runtime_api() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 300;
        let input = crate::app::state::SidebarSnoozeUiState {
            target: crate::app::state::SidebarPaneLifecycleTarget::Local(
                crate::app::state::PaneFocusTarget {
                    workspace_id: app.state.workspaces[0].id.clone(),
                    pane_id,
                },
            ),
            anchor: (0, 0),
            selected: crate::app::state::SidebarSnoozeMenuAction::SetTime,
            time_draft: Some("12:30".into()),
            error: None,
        };
        app.state.sidebar_snooze = Some(input.clone());
        app.dispatch_snooze_time_deadline(input, deadline);
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .and_then(crate::pane::PaneState::snoozed_until),
            Some(deadline)
        );

        let menu = pane_context_menu(&app.state, pane_id, ContextMenuAction::Unsnooze);
        assert!(app
            .state
            .context_menu_items(&menu)
            .contains(&crate::app::state::UNSNOOZE_ITEM));
        app.apply_context_menu_action_via_api(menu, ContextMenuAction::Unsnooze);
        assert!(!app.state.pane_is_snoozed(0, pane_id));
    }

    #[test]
    fn context_menu_state_and_snooze_actions_are_isolated_between_clients() {
        let mut app = app_with_test_workspaces(&["main"]);
        let first_pane = app.state.workspaces[0].tabs[0].root_pane;
        let second_pane = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        let mut client_a = crate::app::state::SidebarPresentationState::default();
        let mut client_b = crate::app::state::SidebarPresentationState::default();

        app.state.swap_sidebar_presentation(&mut client_a);
        app.state.context_menu = Some(pane_context_menu(
            &app.state,
            first_pane,
            ContextMenuAction::Snooze,
        ));
        app.state.open_client_overlay(ClientOverlay::ContextMenu);
        app.state.swap_sidebar_presentation(&mut client_a);

        app.state.swap_sidebar_presentation(&mut client_b);
        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.state.pane_is_snoozed(0, first_pane));
        assert!(!app.state.pane_is_snoozed(0, second_pane));
        app.state.swap_sidebar_presentation(&mut client_b);

        app.state.swap_sidebar_presentation(&mut client_a);
        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        let target = app
            .state
            .sidebar_snooze
            .as_ref()
            .expect("client A snooze menu")
            .target
            .clone();
        assert_eq!(
            target.local().map(|target| target.pane_id),
            Some(first_pane)
        );
        assert!(app
            .handle_sidebar_snooze_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));
        assert!(app.state.pane_is_snoozed(0, first_pane));
        assert!(!app.state.pane_is_snoozed(0, second_pane));

        app.state.context_menu = Some(pane_context_menu(
            &app.state,
            first_pane,
            ContextMenuAction::Unsnooze,
        ));
        app.state.open_client_overlay(ClientOverlay::ContextMenu);
        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.state.pane_is_snoozed(0, first_pane));
        assert!(!app.state.pane_is_snoozed(0, second_pane));

        app.state.context_menu = Some(pane_context_menu(
            &app.state,
            first_pane,
            ContextMenuAction::SetTime,
        ));
        app.state.open_client_overlay(ClientOverlay::ContextMenu);
        app.handle_context_menu_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(app.state.client_overlay, ClientOverlay::None);
        assert_eq!(
            app.state
                .sidebar_snooze
                .as_ref()
                .and_then(|snooze| snooze.target.local().map(|target| target.pane_id)),
            Some(first_pane)
        );
    }

    #[test]
    fn context_menu_hides_snooze_for_settled_and_attention_gated_panes() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        let menu = pane_context_menu(&app.state, pane_id, ContextMenuAction::Snooze);

        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("pane")
            .settled_at = Some(1);
        let settled_actions = app.state.context_menu_actions(&menu);
        assert!(!settled_actions.contains(&ContextMenuAction::Snooze));
        assert!(!settled_actions.contains(&ContextMenuAction::SetTime));

        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("pane")
            .settled_at = None;
        let terminal = app.state.terminals.get_mut(&terminal_id).expect("terminal");
        terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);
        terminal.closing_items = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "Answer".into(),
            text: "Resolve the active gate".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let gated_actions = app.state.context_menu_actions(&menu);
        assert!(!gated_actions.contains(&ContextMenuAction::Snooze));
        assert!(!gated_actions.contains(&ContextMenuAction::SetTime));
    }

    #[tokio::test]
    async fn context_menu_set_time_cancel_and_save_return_input_to_the_pane() {
        let mut app = app_with_test_workspaces(&["main"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, mut pane_input) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 4);
        app.state.workspaces[0].insert_test_runtime(pane_id, runtime);
        app.state.context_menu = Some(pane_context_menu(
            &app.state,
            pane_id,
            ContextMenuAction::SetTime,
        ));
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::ContextMenu);
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()),
            )],
            false,
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        assert!(app
            .state
            .sidebar_snooze
            .as_ref()
            .is_some_and(|snooze| snooze.time_draft.is_some()));
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()),
            )],
            false,
        );
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(KeyCode::Char('c'), KeyModifiers::empty()),
            )],
            false,
        );
        assert_eq!(
            pane_input.try_recv().expect("input after cancel"),
            bytes::Bytes::from_static(b"c")
        );

        app.state.context_menu = Some(pane_context_menu(
            &app.state,
            pane_id,
            ContextMenuAction::SetTime,
        ));
        app.state
            .open_client_overlay(crate::app::state::ClientOverlay::ContextMenu);
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()),
            )],
            false,
        );
        let snooze = app.state.sidebar_snooze.clone().expect("set time editor");
        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 600;
        app.dispatch_snooze_time_deadline(snooze, deadline);
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(KeyCode::Char('s'), KeyModifiers::empty()),
            )],
            false,
        );
        assert_eq!(
            pane_input.try_recv().expect("input after save"),
            bytes::Bytes::from_static(b"s")
        );
    }
}
