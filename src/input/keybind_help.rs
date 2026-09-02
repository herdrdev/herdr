use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::{
    config::{ActionKeybinds, IndexedKeybind, Keybinds},
    input::TerminalKey,
};

pub(crate) type KeybindHelpEntry = (String, Cow<'static, str>);
pub(crate) type KeybindHelpGroup = (&'static str, Vec<KeybindHelpEntry>);

pub(crate) fn keybind_help_text_char(key: &TerminalKey) -> Option<char> {
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

fn entry(key: impl Into<String>, label: &'static str) -> KeybindHelpEntry {
    (key.into(), Cow::Borrowed(label))
}

fn binding_label(bindings: &ActionKeybinds) -> String {
    bindings.label().unwrap_or_else(|| "unset".to_owned())
}

fn indexed_label(bindings: &[IndexedKeybind]) -> String {
    if bindings.is_empty() {
        return "unset".to_owned();
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

fn indexed_range_prefix(bindings: &[IndexedKeybind]) -> Option<&str> {
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

pub(crate) fn keybind_help_groups(
    keybinds: &Keybinds,
    prefix: (crossterm::event::KeyCode, crossterm::event::KeyModifiers),
) -> Vec<KeybindHelpGroup> {
    let mut groups = vec![
        (
            "global",
            vec![
                entry(crate::config::format_key_combo(prefix), "prefix mode"),
                entry(binding_label(&keybinds.help), "keybinds"),
                entry(binding_label(&keybinds.settings), "settings"),
                entry(binding_label(&keybinds.detach), "detach"),
                entry(binding_label(&keybinds.reload_config), "reload config"),
                entry(
                    binding_label(&keybinds.open_notification_target),
                    "open notification target",
                ),
            ],
        ),
        (
            "navigation",
            vec![
                entry("esc", "back"),
                entry(
                    format!(
                        "{} / {}",
                        binding_label(&keybinds.navigate.workspace_up),
                        binding_label(&keybinds.navigate.workspace_down)
                    ),
                    "workspace list",
                ),
                entry(
                    format!(
                        "{} / {} / {} / {} / left / right",
                        binding_label(&keybinds.navigate.pane_left),
                        binding_label(&keybinds.navigate.pane_down),
                        binding_label(&keybinds.navigate.pane_up),
                        binding_label(&keybinds.navigate.pane_right)
                    ),
                    "move focus",
                ),
                entry("tab / shift+tab", "cycle pane"),
                entry("enter", "open workspace"),
                entry("1..9", "switch workspace"),
            ],
        ),
        (
            "workspaces / tabs",
            vec![
                entry(
                    binding_label(&keybinds.workspace_picker),
                    "workspace navigation",
                ),
                entry(binding_label(&keybinds.goto), "session navigator"),
                entry(binding_label(&keybinds.new_workspace), "new workspace"),
                entry(binding_label(&keybinds.new_worktree), "new worktree"),
                entry(binding_label(&keybinds.open_worktree), "open worktree"),
                entry(
                    binding_label(&keybinds.remove_worktree),
                    "delete worktree checkout",
                ),
                entry(
                    binding_label(&keybinds.rename_workspace),
                    "rename workspace",
                ),
                entry(binding_label(&keybinds.close_workspace), "close workspace"),
                entry(
                    binding_label(&keybinds.previous_workspace),
                    "previous workspace",
                ),
                entry(binding_label(&keybinds.next_workspace), "next workspace"),
                entry(
                    indexed_label(&keybinds.switch_workspace),
                    "switch workspace 1-9",
                ),
                entry(binding_label(&keybinds.previous_agent), "previous agent"),
                entry(binding_label(&keybinds.next_agent), "next agent"),
                entry(indexed_label(&keybinds.focus_agent), "focus agent 1-9"),
                entry(binding_label(&keybinds.new_tab), "new tab"),
                entry(binding_label(&keybinds.rename_tab), "rename tab"),
                entry(binding_label(&keybinds.previous_tab), "previous tab"),
                entry(binding_label(&keybinds.next_tab), "next tab"),
                entry(binding_label(&keybinds.move_tab_previous), "move tab left"),
                entry(binding_label(&keybinds.move_tab_next), "move tab right"),
                entry(indexed_label(&keybinds.switch_tab), "switch tab 1-9"),
                entry(binding_label(&keybinds.close_tab), "close tab"),
            ],
        ),
        (
            "panes",
            vec![
                entry(binding_label(&keybinds.split_vertical), "split vertical"),
                entry(
                    binding_label(&keybinds.split_horizontal),
                    "split horizontal",
                ),
                entry(binding_label(&keybinds.close_pane), "close pane"),
                entry(binding_label(&keybinds.rename_pane), "rename pane"),
                entry(binding_label(&keybinds.edit_scrollback), "edit scrollback"),
                entry(binding_label(&keybinds.copy_mode), "copy mode"),
                entry(binding_label(&keybinds.zoom), "zoom pane"),
                entry(binding_label(&keybinds.resize_mode), "resize mode"),
                entry(
                    binding_label(&keybinds.resize_pane_left),
                    "resize pane left",
                ),
                entry(
                    binding_label(&keybinds.resize_pane_down),
                    "resize pane down",
                ),
                entry(binding_label(&keybinds.resize_pane_up), "resize pane up"),
                entry(
                    binding_label(&keybinds.resize_pane_right),
                    "resize pane right",
                ),
                entry(binding_label(&keybinds.toggle_sidebar), "toggle sidebar"),
                entry(binding_label(&keybinds.focus_pane_left), "focus pane left"),
                entry(binding_label(&keybinds.focus_pane_down), "focus pane down"),
                entry(binding_label(&keybinds.focus_pane_up), "focus pane up"),
                entry(
                    binding_label(&keybinds.focus_pane_right),
                    "focus pane right",
                ),
                entry(binding_label(&keybinds.cycle_pane_next), "cycle pane next"),
                entry(
                    binding_label(&keybinds.cycle_pane_previous),
                    "cycle pane previous",
                ),
                entry(binding_label(&keybinds.last_pane), "last pane"),
            ],
        ),
    ];

    if !keybinds.custom_commands.is_empty() {
        groups.push((
            "custom",
            keybinds
                .custom_commands
                .iter()
                .map(|binding| {
                    (
                        binding.label.clone(),
                        binding
                            .description
                            .clone()
                            .map(Cow::Owned)
                            .unwrap_or(Cow::Borrowed("custom command")),
                    )
                })
                .collect(),
        ));
    }
    groups
}

pub(crate) fn filter_keybind_help_groups(
    groups: Vec<KeybindHelpGroup>,
    query: &str,
) -> Vec<KeybindHelpGroup> {
    if query.is_empty() {
        return groups;
    }
    let query = query.to_lowercase();
    groups
        .into_iter()
        .filter_map(|(group, entries)| {
            let entries = entries
                .into_iter()
                .filter(|(key, label)| {
                    key.to_lowercase().contains(&query) || label.to_lowercase().contains(&query)
                })
                .collect::<Vec<_>>();
            (!entries.is_empty()).then_some((group, entries))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<KeybindHelpGroup> {
        vec![
            (
                "workspaces / tabs",
                vec![entry("w", "workspace navigation"), entry("c", "new tab")],
            ),
            (
                "panes",
                vec![entry("v", "split vertical"), entry("x", "close pane")],
            ),
        ]
    }

    #[test]
    fn filter_matches_labels_and_shortcuts_case_insensitively() {
        let filtered = filter_keybind_help_groups(groups(), "WoRk");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1[0].1, "workspace navigation");

        let filtered = filter_keybind_help_groups(groups(), "x");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1[0].1, "close pane");
        assert!(filter_keybind_help_groups(groups(), "panes").is_empty());
    }
}
