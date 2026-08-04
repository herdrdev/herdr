use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    app::state::{AppState, Mode},
    layout::PaneId,
    sidebar_color::{
        SidebarColorPickerState, SidebarColorTarget, SidebarRowColor, SIDEBAR_COLOR_PRESETS,
    },
    terminal::TerminalRuntimeRegistry,
};

use super::modal::leave_modal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SidebarColorAction {
    Preset(usize),
    Save,
    Clear,
    Cancel,
}

fn current_color(state: &AppState, target: &SidebarColorTarget) -> Option<SidebarRowColor> {
    match target {
        SidebarColorTarget::Workspace { workspace_id } => {
            state.workspace_sidebar_colors.get(workspace_id).copied()
        }
        SidebarColorTarget::Tab { tab_id } => state.tab_colors.get(tab_id).copied(),
        SidebarColorTarget::Agent { terminal_id } => {
            state.agent_sidebar_colors.get(terminal_id).copied()
        }
    }
}

fn open_picker(state: &mut AppState, target: SidebarColorTarget, target_label: String) {
    let current = current_color(state, &target);
    let selected_preset = current
        .and_then(|color| {
            SIDEBAR_COLOR_PRESETS
                .iter()
                .position(|(_, preset)| *preset == color)
        })
        .unwrap_or(0);
    let initial = current.unwrap_or(SIDEBAR_COLOR_PRESETS[selected_preset].1);
    state.sidebar_color_picker = Some(SidebarColorPickerState {
        target,
        target_label,
        input: initial.hex(),
        replace_on_type: true,
        selected_preset,
        error: None,
    });
    state.context_menu = None;
    state.mode = Mode::SidebarColor;
}

pub(super) fn open_workspace_color_picker(
    state: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    ws_idx: usize,
) {
    let Some(workspace) = state.workspaces.get(ws_idx) else {
        leave_modal(state);
        return;
    };
    let workspace_id = workspace.id.clone();
    let label = workspace.display_name_from(&state.terminals, terminal_runtimes);
    open_picker(
        state,
        SidebarColorTarget::Workspace { workspace_id },
        format!("workspace: {label}"),
    );
}

pub(super) fn open_agent_color_picker(
    state: &mut AppState,
    ws_idx: usize,
    tab_idx: usize,
    pane_id: PaneId,
) {
    let Some(workspace) = state.workspaces.get(ws_idx) else {
        leave_modal(state);
        return;
    };
    let Some(tab) = workspace.tabs.get(tab_idx) else {
        leave_modal(state);
        return;
    };
    let Some(terminal_id) = tab.terminal_id(pane_id).cloned() else {
        leave_modal(state);
        return;
    };
    let workspace_label = workspace.display_name_from_terminals(&state.terminals);
    let tab_label = workspace
        .tab_display_name(tab_idx)
        .unwrap_or_else(|| "?".to_string());
    let pane_label = state
        .terminals
        .get(&terminal_id)
        .and_then(|terminal| terminal.manual_label.as_deref());
    let target_label = pane_label.map_or_else(
        || format!("agent: {workspace_label} · {tab_label}"),
        |pane| format!("agent: {workspace_label} · {tab_label} · {pane}"),
    );
    open_picker(
        state,
        SidebarColorTarget::Agent { terminal_id },
        target_label,
    );
}

pub(super) fn open_tab_color_picker(state: &mut AppState, ws_idx: usize, tab_idx: usize) {
    let Some(workspace) = state.workspaces.get(ws_idx) else {
        leave_modal(state);
        return;
    };
    let Some(tab) = workspace.tabs.get(tab_idx) else {
        leave_modal(state);
        return;
    };
    let tab_id = crate::workspace::public_tab_id_for_number(&workspace.id, tab.number);
    let workspace_label = workspace.display_name_from_terminals(&state.terminals);
    let tab_label = workspace
        .tab_display_name(tab_idx)
        .unwrap_or_else(|| "?".to_string());
    open_picker(
        state,
        SidebarColorTarget::Tab { tab_id },
        format!("tab: {workspace_label} · {tab_label}"),
    );
}

fn set_target_color(state: &mut AppState, target: &SidebarColorTarget, color: SidebarRowColor) {
    match target {
        SidebarColorTarget::Workspace { workspace_id } => {
            state
                .workspace_sidebar_colors
                .insert(workspace_id.clone(), color);
        }
        SidebarColorTarget::Tab { tab_id } => {
            state.tab_colors.insert(tab_id.clone(), color);
        }
        SidebarColorTarget::Agent { terminal_id } => {
            state
                .agent_sidebar_colors
                .insert(terminal_id.clone(), color);
        }
    }
}

fn clear_target_color(state: &mut AppState, target: &SidebarColorTarget) {
    match target {
        SidebarColorTarget::Workspace { workspace_id } => {
            state.workspace_sidebar_colors.remove(workspace_id);
        }
        SidebarColorTarget::Tab { tab_id } => {
            state.tab_colors.remove(tab_id);
        }
        SidebarColorTarget::Agent { terminal_id } => {
            state.agent_sidebar_colors.remove(terminal_id);
        }
    }
}

pub(super) fn apply_sidebar_color_action(state: &mut AppState, action: SidebarColorAction) {
    match action {
        SidebarColorAction::Preset(index) => {
            let Some((_, color)) = SIDEBAR_COLOR_PRESETS.get(index).copied() else {
                return;
            };
            let Some(target) = state
                .sidebar_color_picker
                .as_ref()
                .map(|picker| picker.target.clone())
            else {
                return;
            };
            set_target_color(state, &target, color);
            state.sidebar_color_picker = None;
            leave_modal(state);
        }
        SidebarColorAction::Save => {
            let Some((target, input)) = state
                .sidebar_color_picker
                .as_ref()
                .map(|picker| (picker.target.clone(), picker.input.clone()))
            else {
                return;
            };
            let Some(color) = SidebarRowColor::parse_hex(&input) else {
                if let Some(picker) = state.sidebar_color_picker.as_mut() {
                    picker.error = Some("enter #RGB or #RRGGBB".to_string());
                    picker.replace_on_type = false;
                }
                return;
            };
            set_target_color(state, &target, color);
            state.sidebar_color_picker = None;
            leave_modal(state);
        }
        SidebarColorAction::Clear => {
            if let Some(target) = state
                .sidebar_color_picker
                .as_ref()
                .map(|picker| picker.target.clone())
            {
                clear_target_color(state, &target);
            }
            state.sidebar_color_picker = None;
            leave_modal(state);
        }
        SidebarColorAction::Cancel => {
            state.sidebar_color_picker = None;
            leave_modal(state);
        }
    }
}

fn replace_input_with_preset(state: &mut AppState, selected: usize) {
    let Some((_, color)) = SIDEBAR_COLOR_PRESETS.get(selected).copied() else {
        return;
    };
    if let Some(picker) = state.sidebar_color_picker.as_mut() {
        picker.selected_preset = selected;
        picker.input = color.hex();
        picker.replace_on_type = true;
        picker.error = None;
    }
}

pub(crate) fn insert_sidebar_color_text(state: &mut AppState, text: &str) {
    let Some(picker) = state.sidebar_color_picker.as_mut() else {
        return;
    };
    if picker.replace_on_type {
        picker.input.clear();
        picker.replace_on_type = false;
    }
    for character in text.chars() {
        if picker.input.len() >= 7 {
            break;
        }
        if (character == '#' && picker.input.is_empty()) || character.is_ascii_hexdigit() {
            picker.input.push(character);
        }
    }
    picker.error = None;
}

pub(crate) fn handle_sidebar_color_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => apply_sidebar_color_action(state, SidebarColorAction::Cancel),
        KeyCode::Enter => apply_sidebar_color_action(state, SidebarColorAction::Save),
        KeyCode::Left | KeyCode::Up => {
            let selected = state
                .sidebar_color_picker
                .as_ref()
                .map(|picker| picker.selected_preset)
                .unwrap_or(0);
            let next = if selected == 0 {
                SIDEBAR_COLOR_PRESETS.len() - 1
            } else {
                selected - 1
            };
            replace_input_with_preset(state, next);
        }
        KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
            let selected = state
                .sidebar_color_picker
                .as_ref()
                .map(|picker| picker.selected_preset)
                .unwrap_or(0);
            replace_input_with_preset(state, (selected + 1) % SIDEBAR_COLOR_PRESETS.len());
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(picker) = state.sidebar_color_picker.as_mut() {
                picker.input.clear();
                picker.replace_on_type = false;
                picker.error = None;
            }
        }
        KeyCode::Backspace => {
            if let Some(picker) = state.sidebar_color_picker.as_mut() {
                if picker.replace_on_type {
                    picker.input.clear();
                    picker.replace_on_type = false;
                } else {
                    picker.input.pop();
                }
                picker.error = None;
            }
        }
        KeyCode::Char(character)
            if key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
                && (character == '#' || character.is_ascii_hexdigit()) =>
        {
            insert_sidebar_color_text(state, &character.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn state_with_workspace() -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("repo")];
        state.active = Some(0);
        state.selected = 0;
        state.ensure_test_terminals();
        state
    }

    #[test]
    fn workspace_picker_accepts_custom_hex_and_clear() {
        let mut state = state_with_workspace();
        let workspace_id = state.workspaces[0].id.clone();
        open_workspace_color_picker(&mut state, &TerminalRuntimeRegistry::new(), 0);

        insert_sidebar_color_text(&mut state, "#123ABC");
        apply_sidebar_color_action(&mut state, SidebarColorAction::Save);
        assert_eq!(
            state.workspace_sidebar_colors.get(&workspace_id),
            Some(&SidebarRowColor::new(0x12, 0x3A, 0xBC))
        );

        open_workspace_color_picker(&mut state, &TerminalRuntimeRegistry::new(), 0);
        apply_sidebar_color_action(&mut state, SidebarColorAction::Clear);
        assert!(!state.workspace_sidebar_colors.contains_key(&workspace_id));
    }

    #[test]
    fn tab_picker_uses_stable_tab_identity_and_clear_restores_default() {
        let mut state = state_with_workspace();
        let tab = &state.workspaces[0].tabs[0];
        let tab_id =
            crate::workspace::public_tab_id_for_number(&state.workspaces[0].id, tab.number);

        open_tab_color_picker(&mut state, 0, 0);
        apply_sidebar_color_action(&mut state, SidebarColorAction::Preset(7));
        assert_eq!(
            state.tab_colors.get(&tab_id),
            Some(&SIDEBAR_COLOR_PRESETS[7].1)
        );

        open_tab_color_picker(&mut state, 0, 0);
        apply_sidebar_color_action(&mut state, SidebarColorAction::Clear);
        assert!(!state.tab_colors.contains_key(&tab_id));
    }

    #[test]
    fn agent_picker_is_keyed_to_the_terminal_and_rejects_invalid_hex() {
        let mut state = state_with_workspace();
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        open_agent_color_picker(&mut state, 0, 0, pane_id);

        insert_sidebar_color_text(&mut state, "#12");
        apply_sidebar_color_action(&mut state, SidebarColorAction::Save);
        assert!(state.agent_sidebar_colors.is_empty());
        assert_eq!(state.mode, Mode::SidebarColor);
        assert!(state
            .sidebar_color_picker
            .as_ref()
            .and_then(|picker| picker.error.as_ref())
            .is_some());

        apply_sidebar_color_action(&mut state, SidebarColorAction::Preset(5));
        assert_eq!(
            state.agent_sidebar_colors.get(&terminal_id),
            Some(&SIDEBAR_COLOR_PRESETS[5].1)
        );
    }
}
