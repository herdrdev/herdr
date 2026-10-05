use std::borrow::Cow;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::release_notes::release_notes_close_button_rect;
use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::widgets::{
    modal_stack_areas, panel_contrast_fg, render_action_button, render_modal_header,
    render_modal_shell,
};
use crate::app::AppState;

pub(super) type HelpEntry = (String, Cow<'static, str>);
pub(super) type HelpGroup = (&'static str, Vec<HelpEntry>);

fn help_entry(key: impl Into<String>, label: &'static str) -> HelpEntry {
    (key.into(), Cow::Borrowed(label))
}

fn keybind_label(bindings: &crate::config::ActionKeybinds) -> String {
    bindings.label().unwrap_or_else(|| "unset".to_string())
}

fn indexed_label(bindings: &[crate::config::IndexedKeybind]) -> String {
    if bindings.is_empty() {
        return "unset".to_string();
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

pub(super) fn keybind_help_groups(app: &AppState) -> Vec<HelpGroup> {
    let kb = &app.keybinds;
    let mut groups = Vec::new();

    groups.push((
        "global",
        vec![
            help_entry(
                crate::config::format_key_combo((app.prefix_code, app.prefix_mods)),
                "prefix mode",
            ),
            help_entry(keybind_label(&kb.help), "keybinds"),
            help_entry(keybind_label(&kb.settings), "settings"),
            help_entry(keybind_label(&kb.command_palette), "command palette"),
            help_entry(keybind_label(&kb.detach), "detach"),
            help_entry(keybind_label(&kb.reload_config), "reload config"),
            help_entry(
                keybind_label(&kb.open_notification_target),
                "open notification target",
            ),
            help_entry(keybind_label(&kb.open_work_link), "open work link picker"),
            help_entry(keybind_label(&kb.copy_work_link), "copy work link picker"),
            help_entry(keybind_label(&kb.copy_work_ticket), "copy work ticket"),
            help_entry(keybind_label(&kb.copy_work_pr), "copy work pull request"),
            help_entry(keybind_label(&kb.copy_work_preview), "copy work preview"),
            help_entry(keybind_label(&kb.toggle_theme), "toggle dark/light theme"),
        ],
    ));

    groups.push((
        "navigation",
        vec![
            help_entry("esc", "back"),
            help_entry(
                format!(
                    "{} / {}",
                    keybind_label(&kb.navigate.workspace_up),
                    keybind_label(&kb.navigate.workspace_down)
                ),
                "workspace list",
            ),
            help_entry(
                format!(
                    "{} / {} / {} / {} / left / right",
                    keybind_label(&kb.navigate.pane_left),
                    keybind_label(&kb.navigate.pane_down),
                    keybind_label(&kb.navigate.pane_up),
                    keybind_label(&kb.navigate.pane_right)
                ),
                "move focus",
            ),
            help_entry("tab / shift+tab", "cycle pane"),
            help_entry("enter", "open workspace"),
            help_entry("1..9", "switch workspace"),
        ],
    ));

    let workspace_tab = vec![
        help_entry(keybind_label(&kb.workspace_picker), "workspace navigation"),
        help_entry(keybind_label(&kb.goto), "session navigator"),
        help_entry(keybind_label(&kb.new_workspace), "new workspace"),
        help_entry(keybind_label(&kb.new_thread), "new thread"),
        help_entry(keybind_label(&kb.new_agent_dock), "spawn dock"),
        help_entry(keybind_label(&kb.agent_finder), "search agents"),
        help_entry(keybind_label(&kb.new_worktree), "new worktree"),
        help_entry(keybind_label(&kb.open_worktree), "open worktree"),
        help_entry(
            keybind_label(&kb.remove_worktree),
            "delete worktree checkout",
        ),
        help_entry(keybind_label(&kb.rename_workspace), "rename workspace"),
        help_entry(keybind_label(&kb.close_workspace), "close workspace"),
        help_entry(keybind_label(&kb.previous_workspace), "previous workspace"),
        help_entry(keybind_label(&kb.next_workspace), "next workspace"),
        help_entry(indexed_label(&kb.switch_workspace), "switch workspace 1-9"),
        help_entry(keybind_label(&kb.previous_agent), "previous agent"),
        help_entry(keybind_label(&kb.next_agent), "next agent"),
        help_entry(keybind_label(&kb.next_review_agent), "next review agent"),
        help_entry(indexed_label(&kb.focus_agent), "focus agent 1-9"),
        help_entry(keybind_label(&kb.new_tab), "new tab"),
        help_entry(keybind_label(&kb.rename_tab), "rename tab"),
        help_entry(keybind_label(&kb.toggle_tab_prio), "toggle tab PRIO"),
        help_entry(keybind_label(&kb.toggle_prio_panel), "toggle PRIO panel"),
        help_entry(
            keybind_label(&kb.previous_window),
            "previous tab needing attention across all Spaces",
        ),
        help_entry(
            keybind_label(&kb.next_window),
            "next tab needing attention across all Spaces",
        ),
        help_entry(
            keybind_label(&kb.previous_any_window),
            "previous tab across all Spaces",
        ),
        help_entry(
            keybind_label(&kb.next_any_window),
            "next tab across all Spaces",
        ),
        help_entry(
            keybind_label(&kb.next_blocked_window),
            "next blocked tab across all Spaces",
        ),
        help_entry(
            keybind_label(&kb.previous_tab),
            "previous tab in this Space",
        ),
        help_entry(keybind_label(&kb.next_tab), "next tab in this Space"),
        help_entry(keybind_label(&kb.move_tab_previous), "move tab previous"),
        help_entry(keybind_label(&kb.move_tab_next), "move tab next"),
        help_entry(indexed_label(&kb.switch_tab), "switch tab 1-9"),
        help_entry(keybind_label(&kb.close_tab), "close tab"),
    ];
    groups.push(("workspaces / tabs", workspace_tab));

    let panes = vec![
        help_entry(keybind_label(&kb.split_vertical), "split vertical"),
        help_entry(keybind_label(&kb.split_horizontal), "split horizontal"),
        help_entry(keybind_label(&kb.close_pane), "close pane"),
        help_entry(keybind_label(&kb.rename_pane), "rename pane"),
        help_entry(keybind_label(&kb.edit_scrollback), "edit scrollback"),
        help_entry(keybind_label(&kb.copy_mode), "copy mode"),
        help_entry(keybind_label(&kb.zoom), "zoom pane"),
        help_entry(keybind_label(&kb.toggle_pin_tab), "pin tab in sidebar"),
        help_entry(keybind_label(&kb.resize_mode), "resize mode"),
        help_entry(keybind_label(&kb.resize_pane_left), "resize pane left"),
        help_entry(keybind_label(&kb.resize_pane_down), "resize pane down"),
        help_entry(keybind_label(&kb.resize_pane_up), "resize pane up"),
        help_entry(keybind_label(&kb.resize_pane_right), "resize pane right"),
        help_entry(keybind_label(&kb.toggle_sidebar), "toggle sidebar"),
        help_entry(keybind_label(&kb.focus_sidebar), "focus sidebar"),
        help_entry(
            keybind_label(&kb.focus_owning_repo_group),
            "collapse other repo groups",
        ),
        help_entry(
            keybind_label(&kb.sidebar_cycle_group_mode),
            "cycle sidebar grouping mode",
        ),
        help_entry(keybind_label(&kb.sidebar_refresh), "sidebar.refresh"),
        help_entry(
            keybind_label(&kb.toggle_blocked_filter),
            "toggle blocked filter (configurable)",
        ),
        help_entry(keybind_label(&kb.toggle_dock), "toggle dock"),
        help_entry(keybind_label(&kb.previous_dock_tab), "previous dock tab"),
        help_entry(keybind_label(&kb.next_dock_tab), "next dock tab"),
        help_entry(keybind_label(&kb.editor_open_repo), "editor.open_repo"),
        help_entry(keybind_label(&kb.edit_scratchpad), "edit scratchpad"),
        help_entry(keybind_label(&kb.show_scratchpad), "show scratchpad"),
        help_entry(keybind_label(&kb.symphony), "open Symphony dashboard"),
        help_entry(keybind_label(&kb.runs), "show fleet Runs section"),
        help_entry(keybind_label(&kb.work), "open work projection view"),
        help_entry(keybind_label(&kb.usage), "open usage view"),
        help_entry(keybind_label(&kb.tickets), "open Linear tickets"),
        help_entry(keybind_label(&kb.missive), "open Missive conversations"),
        help_entry(keybind_label(&kb.inbox), "open blocked inbox"),
        help_entry(keybind_label(&kb.focus_pane_left), "focus pane left"),
        help_entry(keybind_label(&kb.focus_pane_down), "focus pane down"),
        help_entry(keybind_label(&kb.focus_pane_up), "focus pane up"),
        help_entry(keybind_label(&kb.focus_pane_right), "focus pane right"),
        help_entry(keybind_label(&kb.cycle_pane_next), "cycle pane next"),
        help_entry(
            keybind_label(&kb.cycle_pane_previous),
            "cycle pane previous",
        ),
        help_entry(keybind_label(&kb.last_pane), "last pane"),
    ];
    groups.push(("panes", panes));

    groups.push((
        "git",
        vec![
            help_entry(keybind_label(&kb.git_pull), "git pull"),
            help_entry(keybind_label(&kb.git_commit), "git commit"),
            help_entry(keybind_label(&kb.git_push), "git push"),
            help_entry(keybind_label(&kb.git_create_pr), "git create pull request"),
        ],
    ));

    groups.push((
        "dock",
        vec![
            help_entry(keybind_label(&kb.dock_home), "dock: home"),
            help_entry(keybind_label(&kb.dock_terminal), "dock: terminal"),
            help_entry(keybind_label(&kb.dock_files), "dock: files"),
            help_entry(keybind_label(&kb.dock_diff), "dock: diff"),
            help_entry(keybind_label(&kb.dock_pr), "dock: pull requests"),
            help_entry(keybind_label(&kb.dock_linear), "dock: linear"),
            help_entry(keybind_label(&kb.dock_missive), "dock: missive"),
            help_entry(keybind_label(&kb.dock_agents), "dock: agents"),
            help_entry(keybind_label(&kb.dock_shortcuts), "dock: shortcuts"),
            help_entry(keybind_label(&kb.dock_context), "dock: context"),
            help_entry(keybind_label(&kb.dock_symphony), "dock: symphony"),
        ],
    ));

    if !kb.custom_commands.is_empty() {
        groups.push((
            "custom",
            kb.custom_commands
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

    if !kb.user_actions.is_empty() {
        groups.push((
            "User actions",
            kb.user_actions
                .iter()
                .map(|action| (action.key_label(), Cow::Owned(action.name.clone())))
                .collect(),
        ));
    }

    groups
}

fn filter_keybind_help_groups(groups: Vec<HelpGroup>, query: &str) -> Vec<HelpGroup> {
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

pub(crate) fn keybind_help_lines(app: &AppState) -> Vec<(usize, Line<'static>)> {
    let heading_style = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(app.palette.mauve)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.palette.text);

    let groups = filter_keybind_help_groups(keybind_help_groups(app), &app.keybind_help.query);
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| key.chars().count()))
        .max()
        .unwrap_or(8);

    let mut lines = Vec::new();

    if groups.is_empty() {
        let message = " no matching keybinds";
        return vec![(
            message.chars().count(),
            Line::from(Span::styled(
                message,
                Style::default().fg(app.palette.overlay1),
            )),
        )];
    }

    for (group, entries) in groups {
        lines.push((
            group.len() + 1,
            Line::from(vec![Span::styled(format!(" {group}"), heading_style)]),
        ));
        for (key, label) in entries {
            let padded_key = format!(" {:<width$} ", key, width = key_width);
            let width = padded_key.chars().count() + label.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::styled(padded_key, key_style),
                    Span::styled(label.into_owned(), label_style),
                ]),
            ));
        }
        lines.push((0, Line::raw("")));
    }

    lines
}

pub(super) fn render_keybind_help_overlay(app: &AppState, frame: &mut Frame) {
    super::dim_background(frame, frame.area());

    let Some(inner) = render_modal_shell(frame, frame.area(), 76, 22, &app.palette) else {
        return;
    };
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 1, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);

    render_modal_header(frame, header_rows[0], "keybinds", &app.palette);
    render_action_button(
        frame,
        release_notes_close_button_rect(header_rows[0]),
        Some("esc"),
        if app.keybind_help.search_focused {
            "back"
        } else {
            "close"
        },
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    let search_line = if app.keybind_help.search_focused {
        Line::from(vec![
            Span::styled(
                " / ",
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                app.keybind_help.query.as_str(),
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(Span::styled(
            " press / to filter by command or shortcut",
            Style::default().fg(app.palette.overlay0),
        ))
    };
    frame.render_widget(Paragraph::new(search_line), header_rows[1]);

    let body_area = stack.content;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app
            .keybind_help_max_scroll()
            .saturating_sub(app.keybind_help.scroll) as usize,
        max_offset_from_bottom: app.keybind_help_max_scroll() as usize,
        viewport_rows: body_area.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(body_area, metrics);
    let text_area = track
        .map(|_| {
            Rect::new(
                body_area.x,
                body_area.y,
                body_area.width.saturating_sub(1),
                body_area.height,
            )
        })
        .unwrap_or(body_area);

    let body = Paragraph::new(
        keybind_help_lines(app)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((app.keybind_help.scroll, 0));
    frame.render_widget(body, text_area);
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }

    let footer = if app.keybind_help.search_focused {
        Line::from(vec![
            Span::styled(" filter ", Style::default().fg(app.palette.overlay0)),
            Span::styled("type/backspace", Style::default().fg(app.palette.text)),
            Span::styled(" · ", Style::default().fg(app.palette.overlay0)),
            Span::styled("clear ", Style::default().fg(app.palette.overlay0)),
            Span::styled("ctrl+u", Style::default().fg(app.palette.text)),
            Span::styled(" · ", Style::default().fg(app.palette.overlay0)),
            Span::styled("scroll ", Style::default().fg(app.palette.overlay0)),
            Span::styled("↑↓/pgup/pgdn", Style::default().fg(app.palette.text)),
            Span::styled(" · ", Style::default().fg(app.palette.overlay0)),
            Span::styled("back ", Style::default().fg(app.palette.overlay0)),
            Span::styled("esc", Style::default().fg(app.palette.text)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" search ", Style::default().fg(app.palette.overlay0)),
            Span::styled("/", Style::default().fg(app.palette.text)),
            Span::styled(" · ", Style::default().fg(app.palette.overlay0)),
            Span::styled("scroll ", Style::default().fg(app.palette.overlay0)),
            Span::styled("j/k/↑↓/pgup/pgdn", Style::default().fg(app.palette.text)),
            Span::styled(" · ", Style::default().fg(app.palette.overlay0)),
            Span::styled("close ", Style::default().fg(app.palette.overlay0)),
            Span::styled("esc/enter", Style::default().fg(app.palette.text)),
        ])
    };
    frame.render_widget(Paragraph::new(footer), stack.footer.unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<HelpGroup> {
        vec![
            (
                "workspaces / tabs",
                vec![
                    help_entry("w", "workspace navigation"),
                    help_entry("c", "new tab"),
                ],
            ),
            (
                "panes",
                vec![
                    help_entry("v", "split vertical"),
                    help_entry("x", "close pane"),
                ],
            ),
        ]
    }

    #[test]
    fn keybind_help_filter_matches_labels_case_insensitively() {
        let filtered = filter_keybind_help_groups(groups(), "WoRk");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "workspaces / tabs");
        assert_eq!(filtered[0].1.len(), 1);
        assert_eq!(filtered[0].1[0].1, "workspace navigation");
    }

    #[test]
    fn keybind_help_filter_matches_shortcuts_without_matching_group_headings() {
        let filtered = filter_keybind_help_groups(groups(), "x");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "panes");
        assert_eq!(filtered[0].1.len(), 1);
        assert_eq!(filtered[0].1[0].1, "close pane");

        assert!(filter_keybind_help_groups(groups(), "panes").is_empty());
    }

    #[test]
    fn global_window_bindings_appear_in_help() {
        let mut app = AppState::test_new();
        app.keybinds = crate::config::Config::default().keybinds();

        let entries = keybind_help_groups(&app)
            .into_iter()
            .flat_map(|(_, entries)| entries)
            .collect::<Vec<_>>();

        assert!(entries.iter().any(|(key, label)| {
            key == "prefix+p" && label == "previous tab needing attention across all Spaces"
        }));
        assert!(entries.iter().any(|(key, label)| {
            key == "prefix+n" && label == "next tab needing attention across all Spaces"
        }));
        assert!(entries.iter().any(
            |(key, label)| key == "prefix+shift+p" && label == "previous tab across all Spaces"
        ));
        assert!(entries
            .iter()
            .any(|(key, label)| key == "prefix+shift+n" && label == "next tab across all Spaces"));
        assert!(entries.iter().any(|(key, label)| {
            key == "prefix+ctrl+p" && label == "previous tab in this Space"
        }));
        assert!(entries
            .iter()
            .any(|(key, label)| { key == "prefix+ctrl+n" && label == "next tab in this Space" }));
    }

    #[test]
    fn finder_default_and_configurable_blocked_filter_appear_in_help() {
        let mut app = AppState::test_new();
        app.keybinds = crate::config::Config::default().keybinds();

        let entries = keybind_help_groups(&app)
            .into_iter()
            .flat_map(|(_, entries)| entries)
            .collect::<Vec<_>>();

        assert!(entries
            .iter()
            .any(|(key, label)| key == "prefix+f" && label == "search agents"));
        assert!(entries
            .iter()
            .any(|(key, label)| key == "unset" && label == "toggle blocked filter (configurable)"));
    }

    #[test]
    fn repository_editor_action_appears_in_help() {
        let mut app = AppState::test_new();
        app.keybinds.editor_open_repo = crate::config::ActionKeybinds::direct("ctrl+alt+v");

        let entries = keybind_help_groups(&app)
            .into_iter()
            .find(|(title, _)| *title == "panes")
            .expect("panes help group")
            .1;

        assert!(entries
            .iter()
            .any(|(key, label)| key == "ctrl+alt+v" && label == "editor.open_repo"));
    }

    #[test]
    fn user_actions_have_their_own_help_group() {
        let mut app = AppState::test_new();
        app.keybinds.user_actions = vec![crate::config::UserAction {
            name: "test".into(),
            command: "just test".into(),
            bindings: crate::config::ActionKeybinds::prefix("t"),
            run_on_worktree_create: false,
            open_in_bottom_pane: true,
            repo: None,
        }];

        let entries = keybind_help_groups(&app)
            .into_iter()
            .find(|(title, _)| *title == "User actions")
            .expect("user action help group")
            .1;
        assert_eq!(
            entries,
            vec![("prefix+t".into(), Cow::Owned("test".into()))]
        );
    }
}
