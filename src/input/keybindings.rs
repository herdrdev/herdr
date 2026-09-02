use crossterm::event::KeyCode;

use crate::config::{CustomCommandKeybind, Keybinds};

use super::TerminalKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeybindDispatch {
    Direct,
    Prefix,
}

#[derive(Debug, Clone)]
pub(crate) enum KeybindMatch {
    Action(KeybindAction),
    Command(CustomCommandKeybind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeybindAction {
    NewWorkspace,
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
    NewTab,
    RenameTab,
    PreviousTab,
    NextTab,
    MoveTabPrevious,
    MoveTabNext,
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
    ClosePane,
    EditScrollback,
    CopyMode,
    Zoom,
    EnterResizeMode,
    ResizePaneLeft,
    ResizePaneDown,
    ResizePaneUp,
    ResizePaneRight,
    ToggleSidebar,
    CyclePaneNext,
    CyclePanePrevious,
    LastPane,
    Help,
    Settings,
    ReloadConfig,
    OpenNotificationTarget,
    Detach,
    OpenNavigator,
}

pub(crate) fn resolve_direct_binding(
    keybinds: &Keybinds,
    key: &TerminalKey,
) -> Option<KeybindMatch> {
    resolve_exact_binding(keybinds, key, KeybindDispatch::Direct)
}

pub(crate) fn resolve_prefix_binding(
    keybinds: &Keybinds,
    key: &TerminalKey,
) -> Option<KeybindMatch> {
    resolve_exact_binding(keybinds, key, KeybindDispatch::Prefix).or_else(|| {
        generated_character_key(key).and_then(|generated_key| {
            resolve_exact_binding(keybinds, &generated_key, KeybindDispatch::Prefix)
        })
    })
}

pub(crate) fn resolve_non_indexed_action(
    keybinds: &Keybinds,
    key: &TerminalKey,
    dispatch: KeybindDispatch,
) -> Option<KeybindAction> {
    for (bindings, action) in [
        (&keybinds.help, KeybindAction::Help),
        (&keybinds.settings, KeybindAction::Settings),
        (&keybinds.workspace_picker, KeybindAction::WorkspacePicker),
        (&keybinds.new_workspace, KeybindAction::NewWorkspace),
        (&keybinds.new_worktree, KeybindAction::NewWorktree),
        (&keybinds.open_worktree, KeybindAction::OpenWorktree),
        (&keybinds.remove_worktree, KeybindAction::RemoveWorktree),
        (&keybinds.rename_workspace, KeybindAction::RenameWorkspace),
        (&keybinds.close_workspace, KeybindAction::CloseWorkspace),
        (
            &keybinds.previous_workspace,
            KeybindAction::PreviousWorkspace,
        ),
        (&keybinds.next_workspace, KeybindAction::NextWorkspace),
        (&keybinds.previous_agent, KeybindAction::PreviousAgent),
        (&keybinds.next_agent, KeybindAction::NextAgent),
        (&keybinds.new_tab, KeybindAction::NewTab),
        (&keybinds.rename_tab, KeybindAction::RenameTab),
        (&keybinds.previous_tab, KeybindAction::PreviousTab),
        (&keybinds.next_tab, KeybindAction::NextTab),
        (&keybinds.move_tab_previous, KeybindAction::MoveTabPrevious),
        (&keybinds.move_tab_next, KeybindAction::MoveTabNext),
        (&keybinds.close_tab, KeybindAction::CloseTab),
        (&keybinds.rename_pane, KeybindAction::RenamePane),
        (&keybinds.edit_scrollback, KeybindAction::EditScrollback),
        (&keybinds.copy_mode, KeybindAction::CopyMode),
        (&keybinds.focus_pane_left, KeybindAction::FocusPaneLeft),
        (&keybinds.focus_pane_down, KeybindAction::FocusPaneDown),
        (&keybinds.focus_pane_up, KeybindAction::FocusPaneUp),
        (&keybinds.focus_pane_right, KeybindAction::FocusPaneRight),
        (&keybinds.swap_pane_left, KeybindAction::SwapPaneLeft),
        (&keybinds.swap_pane_down, KeybindAction::SwapPaneDown),
        (&keybinds.swap_pane_up, KeybindAction::SwapPaneUp),
        (&keybinds.swap_pane_right, KeybindAction::SwapPaneRight),
        (&keybinds.last_pane, KeybindAction::LastPane),
        (&keybinds.cycle_pane_next, KeybindAction::CyclePaneNext),
        (
            &keybinds.cycle_pane_previous,
            KeybindAction::CyclePanePrevious,
        ),
        (&keybinds.split_vertical, KeybindAction::SplitVertical),
        (&keybinds.split_horizontal, KeybindAction::SplitHorizontal),
        (&keybinds.close_pane, KeybindAction::ClosePane),
        (&keybinds.zoom, KeybindAction::Zoom),
        (&keybinds.resize_mode, KeybindAction::EnterResizeMode),
        (&keybinds.resize_pane_left, KeybindAction::ResizePaneLeft),
        (&keybinds.resize_pane_down, KeybindAction::ResizePaneDown),
        (&keybinds.resize_pane_up, KeybindAction::ResizePaneUp),
        (&keybinds.resize_pane_right, KeybindAction::ResizePaneRight),
        (&keybinds.toggle_sidebar, KeybindAction::ToggleSidebar),
        (&keybinds.reload_config, KeybindAction::ReloadConfig),
        (
            &keybinds.open_notification_target,
            KeybindAction::OpenNotificationTarget,
        ),
        (&keybinds.detach, KeybindAction::Detach),
        (&keybinds.goto, KeybindAction::OpenNavigator),
    ] {
        if action_matches(bindings, key, dispatch) {
            return Some(action);
        }
    }
    None
}

pub(crate) fn resolve_custom_command(
    keybinds: &Keybinds,
    key: &TerminalKey,
    dispatch: KeybindDispatch,
) -> Option<CustomCommandKeybind> {
    keybinds
        .custom_commands
        .iter()
        .find(|binding| match dispatch {
            KeybindDispatch::Direct => binding.bindings.matches_direct_key(key),
            KeybindDispatch::Prefix => binding.bindings.matches_prefix_key(key),
        })
        .cloned()
}

pub(crate) fn resolve_indexed_action(
    keybinds: &Keybinds,
    key: &TerminalKey,
    dispatch: KeybindDispatch,
) -> Option<KeybindAction> {
    let actual_modifiers = crate::config::normalize_key_combo((key.code, key.modifiers)).1;

    for exact_modifiers in [true, false] {
        let trigger_matches = |binding: &crate::config::IndexedKeybind| {
            let dispatch_matches = match dispatch {
                KeybindDispatch::Direct => binding.trigger.is_direct(),
                KeybindDispatch::Prefix => binding.trigger.is_prefix(),
            };
            let expected_modifiers = crate::config::normalize_key_combo(binding.trigger.combo()).1;
            dispatch_matches && (actual_modifiers == expected_modifiers) == exact_modifiers
        };

        for binding in &keybinds.switch_tab {
            if trigger_matches(binding) {
                if let Some(index) = binding.matched_index(key) {
                    return Some(KeybindAction::SwitchTab(index));
                }
            }
        }
        for binding in &keybinds.switch_workspace {
            if trigger_matches(binding) {
                if let Some(index) = binding.matched_index(key) {
                    return Some(KeybindAction::SwitchWorkspace(index));
                }
            }
        }
        for binding in &keybinds.focus_agent {
            if trigger_matches(binding) {
                if let Some(index) = binding.matched_index(key) {
                    return Some(KeybindAction::FocusAgent(index));
                }
            }
        }
    }

    None
}

fn resolve_exact_binding(
    keybinds: &Keybinds,
    key: &TerminalKey,
    dispatch: KeybindDispatch,
) -> Option<KeybindMatch> {
    resolve_non_indexed_action(keybinds, key, dispatch)
        .map(KeybindMatch::Action)
        .or_else(|| resolve_custom_command(keybinds, key, dispatch).map(KeybindMatch::Command))
        .or_else(|| resolve_indexed_action(keybinds, key, dispatch).map(KeybindMatch::Action))
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

fn action_matches(
    bindings: &crate::config::ActionKeybinds,
    key: &TerminalKey,
    dispatch: KeybindDispatch,
) -> bool {
    match dispatch {
        KeybindDispatch::Direct => bindings.matches_direct_key(key),
        KeybindDispatch::Prefix => bindings.matches_prefix_key(key),
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;

    #[test]
    fn one_shared_resolver_handles_direct_prefix_and_indexed_bindings() {
        let keybinds = Keybinds {
            next_tab: crate::config::ActionKeybinds::direct("ctrl+n"),
            ..Keybinds::default()
        };

        let direct = TerminalKey::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(matches!(
            resolve_direct_binding(&keybinds, &direct),
            Some(KeybindMatch::Action(KeybindAction::NextTab))
        ));

        let help = TerminalKey::new(KeyCode::Char('?'), KeyModifiers::empty());
        assert!(matches!(
            resolve_prefix_binding(&keybinds, &help),
            Some(KeybindMatch::Action(KeybindAction::Help))
        ));

        let one = TerminalKey::new(KeyCode::Char('1'), KeyModifiers::empty());
        assert!(matches!(
            resolve_prefix_binding(&keybinds, &one),
            Some(KeybindMatch::Action(KeybindAction::SwitchTab(0)))
        ));
    }

    #[test]
    fn prefix_resolution_uses_shared_generated_character_fallback() {
        let keybinds = Keybinds::default();
        let key = TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
            .with_generated_text(Some("?".to_owned()));

        assert!(matches!(
            resolve_prefix_binding(&keybinds, &key),
            Some(KeybindMatch::Action(KeybindAction::Help))
        ));
    }
}
