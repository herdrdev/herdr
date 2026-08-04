//! Command palette catalog.
//!
//! Every entry pairs a user-visible command name with the action that runs it
//! and, when the command is bound through the `[keys]` config table, the config
//! field that owns its shortcut. The palette is the single place that knows
//! which commands exist, so both the overlay and the inline rebind flow read
//! from here instead of duplicating lists.

use std::borrow::Cow;

use crate::app::input::NavigateAction;
use crate::app::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    /// Runs the same action the bound key would run.
    Navigate(NavigateAction),
    /// Enters navigation mode.
    NavigateMode,
    /// Runs a `[[keys.command]]` entry by index.
    Custom(usize),
    /// Indexed families such as "switch tab 1-9" have no single target, so the
    /// palette exposes them for rebinding only.
    RebindOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaletteCommand {
    pub group: &'static str,
    pub label: Cow<'static, str>,
    pub shortcut: String,
    pub action: PaletteAction,
    /// Field under `[keys]` that owns this shortcut, when it can be rebound
    /// from the palette. Custom commands live in `[[keys.command]]` and are not
    /// rebindable inline.
    pub config_key: Option<&'static str>,
}

impl PaletteCommand {
    pub fn is_executable(&self) -> bool {
        !matches!(self.action, PaletteAction::RebindOnly)
    }

    pub fn is_bound(&self) -> bool {
        self.shortcut != UNSET
    }

    /// Fields the search pattern is matched against, each independently so
    /// anchored patterns like `^split` stay useful.
    fn search_fields(&self) -> [&str; 3] {
        [self.label.as_ref(), self.shortcut.as_str(), self.group]
    }
}

pub(crate) const UNSET: &str = "unset";

fn keybind_label(bindings: &crate::config::ActionKeybinds) -> String {
    bindings.label().unwrap_or_else(|| UNSET.to_string())
}

fn indexed_label(bindings: &[crate::config::IndexedKeybind]) -> String {
    if bindings.is_empty() {
        return UNSET.to_string();
    }

    let mut parts = Vec::new();
    let mut index = 0;
    while index < bindings.len() {
        if let Some(prefix) = indexed_range_prefix(&bindings[index..]) {
            parts.push(format!("{prefix}1..9"));
            index += 9;
        } else {
            parts.push(bindings[index].label.clone());
            index += 1;
        }
    }

    parts.join(" / ")
}

fn indexed_range_prefix(bindings: &[crate::config::IndexedKeybind]) -> Option<&str> {
    let run = bindings.get(..9)?;
    let prefix = run[0].label.strip_suffix('1')?;
    for (offset, binding) in run.iter().enumerate() {
        let digit = char::from(b'1' + offset as u8);
        if binding.label.strip_suffix(digit) != Some(prefix) {
            return None;
        }
    }
    Some(prefix)
}

/// Every command the palette can list, in display order.
pub(crate) fn palette_commands(app: &AppState) -> Vec<PaletteCommand> {
    let kb = &app.keybinds;
    let mut commands = Vec::new();

    let mut push = |group, label: &'static str, shortcut: String, action, config_key| {
        commands.push(PaletteCommand {
            group,
            label: Cow::Borrowed(label),
            shortcut,
            action,
            config_key: Some(config_key),
        });
    };

    const GLOBAL: &str = "global";
    push(
        GLOBAL,
        "navigation mode",
        crate::config::format_key_combo((app.prefix_code, app.prefix_mods)),
        PaletteAction::NavigateMode,
        "prefix",
    );
    push(
        GLOBAL,
        "command palette",
        keybind_label(&kb.commands),
        PaletteAction::Navigate(NavigateAction::CommandPalette),
        "commands",
    );
    push(
        GLOBAL,
        "settings",
        keybind_label(&kb.settings),
        PaletteAction::Navigate(NavigateAction::Settings),
        "settings",
    );
    push(
        GLOBAL,
        "reload config",
        keybind_label(&kb.reload_config),
        PaletteAction::Navigate(NavigateAction::ReloadConfig),
        "reload_config",
    );
    push(
        GLOBAL,
        "open notification target",
        keybind_label(&kb.open_notification_target),
        PaletteAction::Navigate(NavigateAction::OpenNotificationTarget),
        "open_notification_target",
    );
    push(
        GLOBAL,
        "detach",
        keybind_label(&kb.detach),
        PaletteAction::Navigate(NavigateAction::Detach),
        "detach",
    );

    const WORKSPACES: &str = "workspaces / tabs";
    push(
        WORKSPACES,
        "workspace navigation",
        keybind_label(&kb.workspace_picker),
        PaletteAction::Navigate(NavigateAction::WorkspacePicker),
        "workspace_picker",
    );
    push(
        WORKSPACES,
        "session navigator",
        keybind_label(&kb.goto),
        PaletteAction::Navigate(NavigateAction::OpenNavigator),
        "goto",
    );
    push(
        WORKSPACES,
        "new workspace",
        keybind_label(&kb.new_workspace),
        PaletteAction::Navigate(NavigateAction::NewWorkspace),
        "new_workspace",
    );
    push(
        WORKSPACES,
        "new worktree",
        keybind_label(&kb.new_worktree),
        PaletteAction::Navigate(NavigateAction::NewWorktree),
        "new_worktree",
    );
    push(
        WORKSPACES,
        "open worktree",
        keybind_label(&kb.open_worktree),
        PaletteAction::Navigate(NavigateAction::OpenWorktree),
        "open_worktree",
    );
    push(
        WORKSPACES,
        "delete worktree checkout",
        keybind_label(&kb.remove_worktree),
        PaletteAction::Navigate(NavigateAction::RemoveWorktree),
        "remove_worktree",
    );
    push(
        WORKSPACES,
        "rename workspace",
        keybind_label(&kb.rename_workspace),
        PaletteAction::Navigate(NavigateAction::RenameWorkspace),
        "rename_workspace",
    );
    push(
        WORKSPACES,
        "close workspace",
        keybind_label(&kb.close_workspace),
        PaletteAction::Navigate(NavigateAction::CloseWorkspace),
        "close_workspace",
    );
    push(
        WORKSPACES,
        "previous workspace",
        keybind_label(&kb.previous_workspace),
        PaletteAction::Navigate(NavigateAction::PreviousWorkspace),
        "previous_workspace",
    );
    push(
        WORKSPACES,
        "next workspace",
        keybind_label(&kb.next_workspace),
        PaletteAction::Navigate(NavigateAction::NextWorkspace),
        "next_workspace",
    );
    push(
        WORKSPACES,
        "switch workspace 1-9",
        indexed_label(&kb.switch_workspace),
        PaletteAction::RebindOnly,
        "switch_workspace",
    );
    push(
        WORKSPACES,
        "previous agent",
        keybind_label(&kb.previous_agent),
        PaletteAction::Navigate(NavigateAction::PreviousAgent),
        "previous_agent",
    );
    push(
        WORKSPACES,
        "next agent",
        keybind_label(&kb.next_agent),
        PaletteAction::Navigate(NavigateAction::NextAgent),
        "next_agent",
    );
    push(
        WORKSPACES,
        "focus agent 1-9",
        indexed_label(&kb.focus_agent),
        PaletteAction::RebindOnly,
        "focus_agent",
    );
    push(
        WORKSPACES,
        "new tab",
        keybind_label(&kb.new_tab),
        PaletteAction::Navigate(NavigateAction::NewTab),
        "new_tab",
    );
    push(
        WORKSPACES,
        "rename tab",
        keybind_label(&kb.rename_tab),
        PaletteAction::Navigate(NavigateAction::RenameTab),
        "rename_tab",
    );
    push(
        WORKSPACES,
        "previous tab",
        keybind_label(&kb.previous_tab),
        PaletteAction::Navigate(NavigateAction::PreviousTab),
        "previous_tab",
    );
    push(
        WORKSPACES,
        "next tab",
        keybind_label(&kb.next_tab),
        PaletteAction::Navigate(NavigateAction::NextTab),
        "next_tab",
    );
    push(
        WORKSPACES,
        "switch tab 1-9",
        indexed_label(&kb.switch_tab),
        PaletteAction::RebindOnly,
        "switch_tab",
    );
    push(
        WORKSPACES,
        "close tab",
        keybind_label(&kb.close_tab),
        PaletteAction::Navigate(NavigateAction::CloseTab),
        "close_tab",
    );

    const PANES: &str = "panes";
    push(
        PANES,
        "split vertical",
        keybind_label(&kb.split_vertical),
        PaletteAction::Navigate(NavigateAction::SplitVertical),
        "split_vertical",
    );
    push(
        PANES,
        "split horizontal",
        keybind_label(&kb.split_horizontal),
        PaletteAction::Navigate(NavigateAction::SplitHorizontal),
        "split_horizontal",
    );
    push(
        PANES,
        "close pane",
        keybind_label(&kb.close_pane),
        PaletteAction::Navigate(NavigateAction::ClosePane),
        "close_pane",
    );
    push(
        PANES,
        "rename pane",
        keybind_label(&kb.rename_pane),
        PaletteAction::Navigate(NavigateAction::RenamePane),
        "rename_pane",
    );
    push(
        PANES,
        "edit scrollback",
        keybind_label(&kb.edit_scrollback),
        PaletteAction::Navigate(NavigateAction::EditScrollback),
        "edit_scrollback",
    );
    push(
        PANES,
        "copy mode",
        keybind_label(&kb.copy_mode),
        PaletteAction::Navigate(NavigateAction::CopyMode),
        "copy_mode",
    );
    push(
        PANES,
        "zoom pane",
        keybind_label(&kb.zoom),
        PaletteAction::Navigate(NavigateAction::Zoom),
        "zoom",
    );
    push(
        PANES,
        "resize mode",
        keybind_label(&kb.resize_mode),
        PaletteAction::Navigate(NavigateAction::EnterResizeMode),
        "resize_mode",
    );
    push(
        PANES,
        "toggle sidebar",
        keybind_label(&kb.toggle_sidebar),
        PaletteAction::Navigate(NavigateAction::ToggleSidebar),
        "toggle_sidebar",
    );
    push(
        PANES,
        "focus pane left",
        keybind_label(&kb.focus_pane_left),
        PaletteAction::Navigate(NavigateAction::FocusPaneLeft),
        "focus_pane_left",
    );
    push(
        PANES,
        "focus pane down",
        keybind_label(&kb.focus_pane_down),
        PaletteAction::Navigate(NavigateAction::FocusPaneDown),
        "focus_pane_down",
    );
    push(
        PANES,
        "focus pane up",
        keybind_label(&kb.focus_pane_up),
        PaletteAction::Navigate(NavigateAction::FocusPaneUp),
        "focus_pane_up",
    );
    push(
        PANES,
        "focus pane right",
        keybind_label(&kb.focus_pane_right),
        PaletteAction::Navigate(NavigateAction::FocusPaneRight),
        "focus_pane_right",
    );
    push(
        PANES,
        "swap pane left",
        keybind_label(&kb.swap_pane_left),
        PaletteAction::Navigate(NavigateAction::SwapPaneLeft),
        "swap_pane_left",
    );
    push(
        PANES,
        "swap pane down",
        keybind_label(&kb.swap_pane_down),
        PaletteAction::Navigate(NavigateAction::SwapPaneDown),
        "swap_pane_down",
    );
    push(
        PANES,
        "swap pane up",
        keybind_label(&kb.swap_pane_up),
        PaletteAction::Navigate(NavigateAction::SwapPaneUp),
        "swap_pane_up",
    );
    push(
        PANES,
        "swap pane right",
        keybind_label(&kb.swap_pane_right),
        PaletteAction::Navigate(NavigateAction::SwapPaneRight),
        "swap_pane_right",
    );
    push(
        PANES,
        "cycle pane next",
        keybind_label(&kb.cycle_pane_next),
        PaletteAction::Navigate(NavigateAction::CyclePaneNext),
        "cycle_pane_next",
    );
    push(
        PANES,
        "cycle pane previous",
        keybind_label(&kb.cycle_pane_previous),
        PaletteAction::Navigate(NavigateAction::CyclePanePrevious),
        "cycle_pane_previous",
    );
    push(
        PANES,
        "last pane",
        keybind_label(&kb.last_pane),
        PaletteAction::Navigate(NavigateAction::LastPane),
        "last_pane",
    );

    for (idx, binding) in kb.custom_commands.iter().enumerate() {
        commands.push(PaletteCommand {
            group: "custom",
            label: binding
                .description
                .clone()
                .map(Cow::Owned)
                .unwrap_or(Cow::Borrowed("custom command")),
            shortcut: binding.label.clone(),
            action: PaletteAction::Custom(idx),
            config_key: None,
        });
    }

    commands
}

/// Filters commands by a regex over "label shortcut group". An invalid or
/// half-typed pattern degrades to a case-insensitive substring match so the
/// list never blanks out mid-keystroke.
pub(crate) fn filter_palette_commands(
    commands: Vec<PaletteCommand>,
    query: &str,
) -> Vec<PaletteCommand> {
    let query = query.trim();
    if query.is_empty() {
        return commands;
    }

    match regex::RegexBuilder::new(query)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
    {
        Ok(pattern) => commands
            .into_iter()
            .filter(|command| {
                command
                    .search_fields()
                    .iter()
                    .any(|field| pattern.is_match(field))
            })
            .collect(),
        Err(_) => {
            let needle = query.to_lowercase();
            commands
                .into_iter()
                .filter(|command| {
                    command
                        .search_fields()
                        .iter()
                        .any(|field| field.to_lowercase().contains(&needle))
                })
                .collect()
        }
    }
}

/// One rendered line in the palette body. Group headings label the rows below
/// them and are never selectable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaletteLine {
    Heading(&'static str),
    Row(usize),
}

impl PaletteLine {
    pub fn is_heading(&self) -> bool {
        matches!(self, Self::Heading(_))
    }

    pub fn row(&self) -> Option<usize> {
        match self {
            Self::Row(idx) => Some(*idx),
            Self::Heading(_) => None,
        }
    }
}

pub(crate) fn palette_display_lines(commands: &[PaletteCommand]) -> Vec<PaletteLine> {
    let mut lines = Vec::with_capacity(commands.len() + 4);
    let mut current_group = None;
    for (idx, command) in commands.iter().enumerate() {
        if current_group != Some(command.group) {
            current_group = Some(command.group);
            lines.push(PaletteLine::Heading(command.group));
        }
        lines.push(PaletteLine::Row(idx));
    }
    lines
}

/// Display-line index of a command row, used to keep the selection on screen.
pub(crate) fn palette_line_of_row(lines: &[PaletteLine], row: usize) -> Option<usize> {
    lines.iter().position(|line| *line == PaletteLine::Row(row))
}

impl AppState {
    pub(crate) fn palette_filtered_commands(&self) -> Vec<PaletteCommand> {
        filter_palette_commands(palette_commands(self), &self.keybind_help.query)
    }

    pub(crate) fn selected_palette_command(&self) -> Option<PaletteCommand> {
        self.palette_filtered_commands()
            .into_iter()
            .nth(self.keybind_help.selected)
    }

    /// Config field currently bound to `binding`, ignoring `skip_key` so a
    /// command re-binding its own shortcut is not reported as a conflict.
    pub(crate) fn palette_binding_conflict(
        &self,
        binding: &str,
        skip_key: &str,
    ) -> Option<PaletteCommand> {
        palette_commands(self).into_iter().find(|command| {
            command.config_key.is_some_and(|key| key != skip_key)
                && command.shortcut.split(" / ").any(|label| label == binding)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<PaletteCommand> {
        vec![
            PaletteCommand {
                group: "panes",
                label: Cow::Borrowed("split vertical"),
                shortcut: "prefix+v".to_string(),
                action: PaletteAction::RebindOnly,
                config_key: Some("split_vertical"),
            },
            PaletteCommand {
                group: "panes",
                label: Cow::Borrowed("close pane"),
                shortcut: "prefix+x".to_string(),
                action: PaletteAction::RebindOnly,
                config_key: Some("close_pane"),
            },
            PaletteCommand {
                group: "global",
                label: Cow::Borrowed("settings"),
                shortcut: UNSET.to_string(),
                action: PaletteAction::RebindOnly,
                config_key: Some("settings"),
            },
            PaletteCommand {
                group: "panes",
                label: Cow::Borrowed("copy mode"),
                shortcut: "prefix+[".to_string(),
                action: PaletteAction::RebindOnly,
                config_key: Some("copy_mode"),
            },
        ]
    }

    #[test]
    fn palette_filter_matches_regex_case_insensitively() {
        let filtered = filter_palette_commands(commands(), "SPLIT|close");

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].label, "split vertical");
        assert_eq!(filtered[1].label, "close pane");
    }

    #[test]
    fn palette_filter_matches_anchored_patterns() {
        let filtered = filter_palette_commands(commands(), "^set");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].label, "settings");
    }

    #[test]
    fn palette_filter_matches_shortcuts() {
        let filtered = filter_palette_commands(commands(), "prefix\\+x");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].label, "close pane");
    }

    #[test]
    fn palette_filter_falls_back_to_substring_for_invalid_regex() {
        // "prefix+[" is an unterminated character class, and also a real shortcut.
        let filtered = filter_palette_commands(commands(), "prefix+[");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].label, "copy mode");
    }

    #[test]
    fn palette_display_lines_group_rows_under_headings() {
        let lines = palette_display_lines(&commands());

        assert_eq!(
            lines,
            vec![
                PaletteLine::Heading("panes"),
                PaletteLine::Row(0),
                PaletteLine::Row(1),
                PaletteLine::Heading("global"),
                PaletteLine::Row(2),
                PaletteLine::Heading("panes"),
                PaletteLine::Row(3),
            ]
        );
        assert_eq!(palette_line_of_row(&lines, 2), Some(4));
    }

    #[test]
    fn palette_catalog_covers_every_command_group() {
        let app = AppState::test_new();
        let commands = palette_commands(&app);

        for group in ["global", "workspaces / tabs", "panes"] {
            assert!(
                commands.iter().any(|command| command.group == group),
                "missing group {group}"
            );
        }
        assert!(commands
            .iter()
            .all(|command| !command.label.is_empty() && !command.shortcut.is_empty()));
    }

    #[test]
    fn palette_reports_conflicting_binding_owner() {
        let app = AppState::test_new();
        let split = palette_commands(&app)
            .into_iter()
            .find(|command| command.label == "split vertical")
            .expect("split vertical command");

        let conflict = app
            .palette_binding_conflict(&split.shortcut, "close_pane")
            .expect("conflict");
        assert_eq!(conflict.label, "split vertical");

        assert!(app
            .palette_binding_conflict(&split.shortcut, "split_vertical")
            .is_none());
    }
}
