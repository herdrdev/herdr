//! The Keybindings section of the settings screen.
//!
//! Every editable row names exactly one place in the config file: a built-in
//! action under `[keys]`, or a user action's `key` inside its `[[actions]]`
//! table. The row list is pure data so the renderer, the hit test, the capture
//! handler and the tests all read the same table and a row can never drift
//! away from the key it claims to edit.

use crate::{
    app::state::AppState,
    config::{ActionKeybinds, Keybinds},
};

/// Where a captured chord is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeybindTarget {
    /// A field of `[keys]`, named exactly as it appears in the config file.
    BuiltIn { field: &'static str },
    /// A 6b user action, addressed by name because `[[actions]]` order is the
    /// operator's, not ours.
    UserAction { name: String },
}

/// One row of the keybindings table. Headings are not selectable targets, they
/// are only there so the table reads the way the help overlay does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeybindingRow {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) heading: bool,
    /// `None` on headings and on rows the settings screen cannot rebind.
    pub(crate) target: Option<KeybindTarget>,
}

pub(crate) type Accessor = fn(&Keybinds) -> &ActionKeybinds;

/// `(config field under [keys], display label, live binding)`.
struct BuiltIn {
    field: &'static str,
    label: &'static str,
    binding: Accessor,
}

const fn built_in(field: &'static str, label: &'static str, binding: Accessor) -> BuiltIn {
    BuiltIn {
        field,
        label,
        binding,
    }
}

/// The built-in table, grouped the way the help overlay groups it.
const BUILT_IN_GROUPS: &[(&str, &[BuiltIn])] = &[
    (
        "global",
        &[
            built_in("help", "keybinds", |kb| &kb.help),
            built_in("settings", "settings", |kb| &kb.settings),
            built_in("command_palette", "command palette", |kb| {
                &kb.command_palette
            }),
            built_in("detach", "detach", |kb| &kb.detach),
            built_in("reload_config", "reload config", |kb| &kb.reload_config),
            built_in(
                "open_notification_target",
                "open notification target",
                |kb| &kb.open_notification_target,
            ),
            built_in("open_work_link", "open work link picker", |kb| {
                &kb.open_work_link
            }),
            built_in("copy_work_link", "copy work link picker", |kb| {
                &kb.copy_work_link
            }),
            built_in("copy_work_ticket", "copy work ticket", |kb| {
                &kb.copy_work_ticket
            }),
            built_in("copy_work_pr", "copy work pull request", |kb| {
                &kb.copy_work_pr
            }),
            built_in("copy_work_preview", "copy work preview", |kb| {
                &kb.copy_work_preview
            }),
            built_in("toggle_theme", "toggle dark/light theme", |kb| {
                &kb.toggle_theme
            }),
        ],
    ),
    (
        "navigation",
        &[
            built_in("navigate_workspace_up", "workspace up", |kb| {
                &kb.navigate.workspace_up
            }),
            built_in("navigate_workspace_down", "workspace down", |kb| {
                &kb.navigate.workspace_down
            }),
            built_in("navigate_pane_left", "focus pane left", |kb| {
                &kb.navigate.pane_left
            }),
            built_in("navigate_pane_down", "focus pane down", |kb| {
                &kb.navigate.pane_down
            }),
            built_in("navigate_pane_up", "focus pane up", |kb| {
                &kb.navigate.pane_up
            }),
            built_in("navigate_pane_right", "focus pane right", |kb| {
                &kb.navigate.pane_right
            }),
        ],
    ),
    (
        "workspaces",
        &[
            built_in("workspace_picker", "workspace navigation", |kb| {
                &kb.workspace_picker
            }),
            built_in("goto", "session navigator", |kb| &kb.goto),
            built_in("new_workspace", "new workspace", |kb| &kb.new_workspace),
            built_in("new_thread", "new thread", |kb| &kb.new_thread),
            built_in("new_agent_dock", "spawn dock", |kb| &kb.new_agent_dock),
            built_in("new_worktree", "new worktree", |kb| &kb.new_worktree),
            built_in("open_worktree", "open worktree", |kb| &kb.open_worktree),
            built_in("remove_worktree", "remove worktree", |kb| {
                &kb.remove_worktree
            }),
            built_in("rename_workspace", "rename workspace", |kb| {
                &kb.rename_workspace
            }),
            built_in("close_workspace", "close workspace", |kb| {
                &kb.close_workspace
            }),
            built_in("previous_workspace", "previous workspace", |kb| {
                &kb.previous_workspace
            }),
            built_in("next_workspace", "next workspace", |kb| &kb.next_workspace),
        ],
    ),
    (
        "tabs",
        &[
            built_in("new_tab", "new tab", |kb| &kb.new_tab),
            built_in("rename_tab", "rename tab", |kb| &kb.rename_tab),
            built_in("close_tab", "close tab", |kb| &kb.close_tab),
            built_in("toggle_tab_prio", "toggle tab prio", |kb| {
                &kb.toggle_tab_prio
            }),
            built_in("toggle_pin_tab", "pin tab", |kb| &kb.toggle_pin_tab),
            built_in("previous_tab", "previous tab", |kb| &kb.previous_tab),
            built_in("next_tab", "next tab", |kb| &kb.next_tab),
            built_in("move_tab_previous", "move tab previous", |kb| {
                &kb.move_tab_previous
            }),
            built_in("move_tab_next", "move tab next", |kb| &kb.move_tab_next),
            built_in("previous_window", "previous window", |kb| {
                &kb.previous_window
            }),
            built_in("next_window", "next window", |kb| &kb.next_window),
            built_in("next_blocked_window", "next blocked window", |kb| {
                &kb.next_blocked_window
            }),
        ],
    ),
    (
        "panes",
        &[
            built_in("focus_pane_left", "focus pane left", |kb| {
                &kb.focus_pane_left
            }),
            built_in("focus_pane_down", "focus pane down", |kb| {
                &kb.focus_pane_down
            }),
            built_in("focus_pane_up", "focus pane up", |kb| &kb.focus_pane_up),
            built_in("focus_pane_right", "focus pane right", |kb| {
                &kb.focus_pane_right
            }),
            built_in("swap_pane_left", "swap pane left", |kb| &kb.swap_pane_left),
            built_in("swap_pane_down", "swap pane down", |kb| &kb.swap_pane_down),
            built_in("swap_pane_up", "swap pane up", |kb| &kb.swap_pane_up),
            built_in("swap_pane_right", "swap pane right", |kb| {
                &kb.swap_pane_right
            }),
            built_in("cycle_pane_next", "cycle pane next", |kb| {
                &kb.cycle_pane_next
            }),
            built_in("cycle_pane_previous", "cycle pane previous", |kb| {
                &kb.cycle_pane_previous
            }),
            built_in("last_pane", "last pane", |kb| &kb.last_pane),
            built_in("split_vertical", "split vertical", |kb| &kb.split_vertical),
            built_in("split_horizontal", "split horizontal", |kb| {
                &kb.split_horizontal
            }),
            built_in("split_left", "split left", |kb| &kb.split_left),
            built_in("split_up", "split up", |kb| &kb.split_up),
            built_in("close_pane", "close pane", |kb| &kb.close_pane),
            built_in("zoom", "zoom pane", |kb| &kb.zoom),
            built_in("resize_mode", "resize mode", |kb| &kb.resize_mode),
            built_in("resize_pane_left", "resize pane left", |kb| {
                &kb.resize_pane_left
            }),
            built_in("resize_pane_down", "resize pane down", |kb| {
                &kb.resize_pane_down
            }),
            built_in("resize_pane_up", "resize pane up", |kb| &kb.resize_pane_up),
            built_in("resize_pane_right", "resize pane right", |kb| {
                &kb.resize_pane_right
            }),
            built_in("rename_pane", "rename pane", |kb| &kb.rename_pane),
            built_in("edit_scrollback", "edit scrollback", |kb| {
                &kb.edit_scrollback
            }),
            built_in("copy_mode", "copy mode", |kb| &kb.copy_mode),
        ],
    ),
    (
        "surfaces",
        &[
            built_in("toggle_sidebar", "toggle sidebar", |kb| &kb.toggle_sidebar),
            built_in("focus_sidebar", "focus sidebar", |kb| &kb.focus_sidebar),
            built_in("assign_pane_to_pod", "assign pane to pod", |kb| {
                &kb.assign_pane_to_pod
            }),
            built_in("focus_owning_repo_group", "focus owning repo group", |kb| {
                &kb.focus_owning_repo_group
            }),
            built_in("sidebar_cycle_group_mode", "cycle sidebar grouping", |kb| {
                &kb.sidebar_cycle_group_mode
            }),
            built_in("sidebar_refresh", "sidebar.refresh", |kb| {
                &kb.sidebar_refresh
            }),
            built_in("toggle_blocked_filter", "toggle blocked filter", |kb| {
                &kb.toggle_blocked_filter
            }),
            built_in("toggle_prio_panel", "toggle prio panel", |kb| {
                &kb.toggle_prio_panel
            }),
            built_in("toggle_dock", "toggle dock", |kb| &kb.toggle_dock),
            built_in("previous_dock_tab", "previous dock tab", |kb| {
                &kb.previous_dock_tab
            }),
            built_in("next_dock_tab", "next dock tab", |kb| &kb.next_dock_tab),
            built_in("editor_open_repo", "editor.open_repo", |kb| {
                &kb.editor_open_repo
            }),
            built_in("toggle_status_detail", "toggle status detail", |kb| {
                &kb.toggle_status_detail
            }),
            built_in("edit_scratchpad", "edit scratchpad", |kb| {
                &kb.edit_scratchpad
            }),
            built_in("show_scratchpad", "show scratchpad", |kb| {
                &kb.show_scratchpad
            }),
            built_in("toggle_notepad", "focus notepad", |kb| &kb.toggle_notepad),
            built_in("toggle_pomodoro", "pause/resume break timer", |kb| {
                &kb.toggle_pomodoro
            }),
            built_in("home", "home", |kb| &kb.home),
            built_in("work", "work", |kb| &kb.work),
            built_in("usage", "usage", |kb| &kb.usage),
            built_in("tickets", "tickets", |kb| &kb.tickets),
            built_in("inbox", "inbox", |kb| &kb.inbox),
            built_in("missive", "missive", |kb| &kb.missive),
            built_in("symphony", "symphony", |kb| &kb.symphony),
            built_in("runs", "fleet runs", |kb| &kb.runs),
            built_in("next_review_agent", "next review agent", |kb| {
                &kb.next_review_agent
            }),
            built_in("previous_agent", "previous agent", |kb| &kb.previous_agent),
            built_in("next_agent", "next agent", |kb| &kb.next_agent),
        ],
    ),
    (
        "git",
        &[
            built_in("git_pull", "git pull", |kb| &kb.git_pull),
            built_in("git_commit", "git commit", |kb| &kb.git_commit),
            built_in("git_push", "git push", |kb| &kb.git_push),
            built_in("git_create_pr", "git create pull request", |kb| {
                &kb.git_create_pr
            }),
        ],
    ),
    (
        "dock",
        &[
            built_in("dock_home", "dock: home", |kb| &kb.dock_home),
            built_in("dock_terminal", "dock: terminal", |kb| &kb.dock_terminal),
            built_in("dock_files", "dock: files", |kb| &kb.dock_files),
            built_in("dock_diff", "dock: diff", |kb| &kb.dock_diff),
            built_in("dock_pr", "dock: pull requests", |kb| &kb.dock_pr),
            built_in("dock_linear", "dock: linear", |kb| &kb.dock_linear),
            built_in("dock_missive", "dock: missive", |kb| &kb.dock_missive),
            built_in("dock_agents", "dock: agents", |kb| &kb.dock_agents),
            built_in("dock_shortcuts", "dock: shortcuts", |kb| &kb.dock_shortcuts),
            built_in("dock_context", "dock: context", |kb| &kb.dock_context),
            built_in("dock_symphony", "dock: symphony", |kb| &kb.dock_symphony),
        ],
    ),
];

/// The label shown when an action has no chord at all.
/// `(group, config field, display label, live binding)` for every built-in row,
/// in table order. The command palette reads this so a command can never claim a
/// label or a key the settings screen does not agree with.
pub(crate) fn built_in_keybinding_entries(
) -> impl Iterator<Item = (&'static str, &'static str, &'static str, Accessor)> {
    BUILT_IN_GROUPS.iter().flat_map(|(group, entries)| {
        entries
            .iter()
            .map(move |entry| (*group, entry.field, entry.label, entry.binding))
    })
}

pub(crate) const UNSET: &str = "unset";

/// The action-to-key table: built-in actions first, then the 6b user actions.
pub(crate) fn settings_keybinding_rows(app: &AppState) -> Vec<KeybindingRow> {
    let mut rows = Vec::new();
    for (group, entries) in BUILT_IN_GROUPS {
        rows.push(KeybindingRow {
            key: String::new(),
            label: (*group).to_string(),
            heading: true,
            target: None,
        });
        for entry in *entries {
            rows.push(KeybindingRow {
                key: (entry.binding)(&app.keybinds)
                    .label()
                    .unwrap_or_else(|| UNSET.to_string()),
                label: entry.label.to_string(),
                heading: false,
                target: Some(KeybindTarget::BuiltIn { field: entry.field }),
            });
        }
    }

    rows.push(KeybindingRow {
        key: String::new(),
        label: "user actions".to_string(),
        heading: true,
        target: None,
    });
    if app.keybinds.user_actions.is_empty() {
        rows.push(KeybindingRow {
            key: String::new(),
            label: "none configured".to_string(),
            heading: true,
            target: None,
        });
    }
    for action in &app.keybinds.user_actions {
        rows.push(KeybindingRow {
            key: action.bindings.label().unwrap_or_else(|| UNSET.to_string()),
            label: action.name.clone(),
            heading: false,
            target: Some(KeybindTarget::UserAction {
                name: action.name.clone(),
            }),
        });
    }
    rows
}

/// The target of `row`, when that row can be rebound.
pub(crate) fn keybinding_target(app: &AppState, row: usize) -> Option<KeybindTarget> {
    settings_keybinding_rows(app)
        .into_iter()
        .nth(row)
        .and_then(|row| row.target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_row_names_a_distinct_keys_field() {
        let mut fields = BUILT_IN_GROUPS
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|entry| entry.field))
            .collect::<Vec<_>>();
        let total = fields.len();
        fields.sort_unstable();
        fields.dedup();
        assert_eq!(fields.len(), total, "duplicate [keys] field in the table");
    }

    #[test]
    fn the_table_lists_built_ins_and_user_actions_with_their_keys() {
        let app = AppState::test_new();
        let rows = settings_keybinding_rows(&app);

        let settings_row = rows
            .iter()
            .find(|row| row.label == "settings")
            .expect("settings row");
        assert_eq!(
            settings_row.target,
            Some(KeybindTarget::BuiltIn { field: "settings" })
        );
        assert!(!settings_row.key.is_empty());
        assert!(rows
            .iter()
            .any(|row| row.heading && row.label == "user actions"));
        assert!(rows.iter().any(|row| {
            row.label == "editor.open_repo"
                && row.target
                    == Some(KeybindTarget::BuiltIn {
                        field: "editor_open_repo",
                    })
        }));
    }
}
