use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::release_notes::release_notes_close_button_rect;
use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::text::truncate_end;
use super::widgets::{
    modal_stack_areas, panel_contrast_fg, render_action_button, render_modal_header,
    render_modal_shell,
};
use crate::app::palette::{palette_commands, palette_display_lines, PaletteCommand, PaletteLine};
use crate::app::AppState;

const SHORTCUT_COLUMN_MAX: usize = 24;
const CAPTURE_PROMPT: &str = "press shortcut…";

/// Width of the shortcut column, measured across the unfiltered catalog so the
/// column does not jump around while the query changes.
fn shortcut_column_width(app: &AppState) -> usize {
    palette_commands(app)
        .iter()
        .map(|command| command.shortcut.chars().count())
        .max()
        .unwrap_or(8)
        .max(CAPTURE_PROMPT.chars().count())
        .min(SHORTCUT_COLUMN_MAX)
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

    render_modal_header(frame, header_rows[0], "commands", &app.palette);
    render_action_button(
        frame,
        release_notes_close_button_rect(header_rows[0]),
        Some("esc"),
        "close",
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_search(app, frame, header_rows[1]);

    let commands = app.palette_filtered_commands();
    let lines = palette_display_lines(&commands);
    let body_area = stack.content;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app.keybind_help_max_scroll()
            - app.keybind_help.scroll.min(app.keybind_help_max_scroll()),
        max_offset_from_bottom: app.keybind_help_max_scroll(),
        viewport_rows: body_area.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(body_area, metrics);
    let list_area = track
        .map(|_| {
            Rect::new(
                body_area.x,
                body_area.y,
                body_area.width.saturating_sub(1),
                body_area.height,
            )
        })
        .unwrap_or(body_area);

    if commands.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no commands match this search",
                Style::default().fg(app.palette.overlay1),
            ))),
            list_area,
        );
    } else {
        render_rows(app, &commands, &lines, frame, list_area);
    }

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

    render_footer(app, frame, stack.footer.unwrap_or_default());
}

fn render_search(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let mut spans = vec![Span::styled(
        " › ",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    )];
    if app.keybind_help.query.is_empty() {
        spans.push(Span::styled(
            "type to search commands",
            Style::default().fg(p.overlay0),
        ));
    } else {
        spans.push(Span::styled(
            app.keybind_help.query.clone(),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ));
        if app.keybind_help.capture.is_none() {
            spans.push(Span::styled(
                "█",
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_rows(
    app: &AppState,
    commands: &[PaletteCommand],
    lines: &[PaletteLine],
    frame: &mut Frame,
    area: Rect,
) {
    let start = app.keybind_help.scroll.min(lines.len());
    let end = lines.len().min(start.saturating_add(area.height as usize));
    let key_width = shortcut_column_width(app);

    for (visible_idx, line) in lines[start..end].iter().enumerate() {
        let rect = Rect::new(area.x, area.y + visible_idx as u16, area.width, 1);
        match line {
            PaletteLine::Heading(group) => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!(" {group}"),
                        Style::default()
                            .fg(app.palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ))),
                    rect,
                );
            }
            PaletteLine::Row(idx) => {
                let Some(command) = commands.get(*idx) else {
                    continue;
                };
                render_row(
                    app,
                    frame,
                    rect,
                    command,
                    key_width,
                    *idx == app.keybind_help.selected,
                );
            }
        }
    }
}

fn render_row(
    app: &AppState,
    frame: &mut Frame,
    rect: Rect,
    command: &PaletteCommand,
    key_width: usize,
    selected: bool,
) {
    let p = &app.palette;
    frame.render_widget(Clear, rect);
    let base_style = if selected {
        Style::default().bg(p.accent).fg(panel_contrast_fg(p))
    } else {
        Style::default().bg(p.panel_bg).fg(p.text)
    };

    let capture = app
        .keybind_help
        .capture
        .as_ref()
        .filter(|_| selected)
        .filter(|capture| Some(capture.config_key) == command.config_key);

    let (shortcut, shortcut_style) = match capture {
        Some(capture) => {
            let text = match &capture.pending_conflict {
                Some(pending) => pending.binding.clone(),
                None => CAPTURE_PROMPT.to_string(),
            };
            (
                text,
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.mauve)
                    .add_modifier(Modifier::BOLD),
            )
        }
        None if !command.is_bound() => (
            command.shortcut.clone(),
            if selected {
                base_style
            } else {
                Style::default().fg(p.overlay0).bg(p.panel_bg)
            },
        ),
        None => (
            command.shortcut.clone(),
            if selected {
                base_style.add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(p.mauve)
                    .bg(p.panel_bg)
                    .add_modifier(Modifier::BOLD)
            },
        ),
    };

    let shortcut = truncate_end(&shortcut, key_width);
    let pad = key_width.saturating_sub(shortcut.chars().count());
    let label_budget = rect
        .width
        .saturating_sub(key_width as u16)
        .saturating_sub(4) as usize;
    let label_style = if selected {
        base_style.add_modifier(Modifier::BOLD)
    } else if command.is_executable() {
        Style::default().fg(p.text).bg(p.panel_bg)
    } else {
        Style::default().fg(p.subtext0).bg(p.panel_bg)
    };

    let spans = vec![
        Span::styled(if selected { " ❯ " } else { "   " }.to_string(), base_style),
        Span::styled(shortcut, shortcut_style),
        Span::styled(" ".repeat(pad + 1), base_style),
        Span::styled(
            truncate_end(command.label.as_ref(), label_budget),
            label_style,
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)).style(base_style), rect);
}

fn render_footer(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let dim = Style::default().fg(p.overlay0);
    let key = Style::default().fg(p.text);

    let line = match app.keybind_help.capture.as_ref() {
        Some(capture) => match &capture.pending_conflict {
            Some(pending) => Line::from(vec![
                Span::styled(
                    format!(" {} ", pending.binding),
                    Style::default().fg(p.peach),
                ),
                Span::styled("is bound to ", dim),
                Span::styled(pending.owner_label.clone(), key),
                Span::styled(" · press again to reassign · ", dim),
                Span::styled("esc", key),
                Span::styled(" cancel", dim),
            ]),
            None => Line::from(vec![
                Span::styled(format!(" shortcut for {} ", capture.command_label), dim),
                Span::styled("· plain keys bind as prefix+key · ", dim),
                Span::styled("esc", key),
                Span::styled(" cancel", dim),
            ]),
        },
        None => match app.keybind_help.notice.as_ref() {
            Some(notice) => Line::from(vec![Span::styled(
                format!(" {notice}"),
                Style::default().fg(p.accent),
            )]),
            None => Line::from(vec![
                Span::styled(" select ", dim),
                Span::styled("↑↓", key),
                Span::styled(" · run ", dim),
                Span::styled("⏎", key),
                Span::styled(" · shortcut ", dim),
                Span::styled("ctrl+s", key),
                Span::styled(" · clear ", dim),
                Span::styled("ctrl+x", key),
                Span::styled(" · close ", dim),
                Span::styled("esc", key),
            ]),
        },
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::palette::{filter_palette_commands, PaletteAction};
    use ratatui::{backend::TestBackend, Terminal};

    fn rendered_palette(app: &AppState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| render_keybind_help_overlay(app, frame))
            .expect("draw palette");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn palette_renders_search_rows_and_footer() {
        let mut app = AppState::test_new();
        app.view.terminal_area = Rect::new(0, 0, 100, 30);
        app.keybind_help.query = "split".to_string();

        let rendered = rendered_palette(&app, 100, 30);

        assert!(rendered.contains("commands"));
        assert!(rendered.contains("split vertical"));
        assert!(rendered.contains("prefix+v"));
        assert!(rendered.contains("panes"));
        assert!(rendered.contains("ctrl+s"));
        assert!(rendered.contains("esc"));
        // Filtered out by the query.
        assert!(!rendered.contains("new tab"));
    }

    #[test]
    fn palette_renders_capture_prompt_and_conflict_on_the_selected_row() {
        let mut app = AppState::test_new();
        app.view.terminal_area = Rect::new(0, 0, 100, 30);
        app.keybind_help.query = "^open worktree$".to_string();
        app.keybind_help.capture = Some(crate::app::state::ShortcutCapture {
            config_key: "open_worktree",
            command_label: "open worktree".to_string(),
            pending_conflict: None,
        });

        let rendered = rendered_palette(&app, 100, 30);
        assert!(rendered.contains(CAPTURE_PROMPT));
        assert!(rendered.contains("plain keys bind as prefix+key"));

        app.keybind_help.capture = Some(crate::app::state::ShortcutCapture {
            config_key: "open_worktree",
            command_label: "open worktree".to_string(),
            pending_conflict: Some(crate::app::state::PendingShortcutConflict {
                binding: "prefix+v".to_string(),
                owner_config_key: "split_vertical",
                owner_label: "split vertical".to_string(),
            }),
        });

        let rendered = rendered_palette(&app, 100, 30);
        assert!(rendered.contains("is bound to"));
        assert!(rendered.contains("press again to reassign"));
    }

    #[test]
    fn palette_renders_an_empty_state_for_a_query_with_no_matches() {
        let mut app = AppState::test_new();
        app.view.terminal_area = Rect::new(0, 0, 100, 30);
        app.keybind_help.query = "zzzz-no-such-command".to_string();

        let rendered = rendered_palette(&app, 100, 30);

        assert!(rendered.contains("no commands match this search"));
    }

    #[test]
    fn shortcut_column_fits_the_capture_prompt() {
        let app = AppState::test_new();
        assert!(shortcut_column_width(&app) >= CAPTURE_PROMPT.chars().count());
        assert!(shortcut_column_width(&app) <= SHORTCUT_COLUMN_MAX);
    }

    #[test]
    fn palette_lists_unset_optional_commands() {
        let app = AppState::test_new();
        let commands = palette_commands(&app);

        for label in [
            "previous agent",
            "next agent",
            "focus agent 1-9",
            "switch workspace 1-9",
            "previous workspace",
            "next workspace",
            "previous tab",
            "next tab",
        ] {
            let command = commands
                .iter()
                .find(|command| command.label == label)
                .unwrap_or_else(|| panic!("missing {label}"));
            assert!(!command.is_bound(), "{label} should be unset by default");
        }

        for (label, shortcut) in [
            ("focus pane left", "prefix+n"),
            ("focus pane down", "prefix+e"),
            ("focus pane up", "prefix+i"),
            ("focus pane right", "prefix+a"),
        ] {
            let command = commands
                .iter()
                .find(|command| command.label == label)
                .unwrap_or_else(|| panic!("missing {label}"));
            assert_eq!(command.shortcut, shortcut);
        }
    }

    #[test]
    fn palette_lists_custom_command_descriptions() {
        let mut app = AppState::test_new();
        app.keybinds.custom_commands = vec![
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+g"),
                label: "prefix+alt+g".to_string(),
                command: "lazygit".to_string(),
                action: crate::config::CustomCommandAction::Pane,
                description: Some("open lazygit".to_string()),
                width: None,
                height: None,
            },
            crate::config::CustomCommandKeybind {
                bindings: crate::config::ActionKeybinds::prefix("alt+h"),
                label: "prefix+alt+h".to_string(),
                command: "echo hello".to_string(),
                action: crate::config::CustomCommandAction::Shell,
                description: None,
                width: None,
                height: None,
            },
        ];

        let commands = palette_commands(&app);
        let custom: Vec<_> = commands
            .iter()
            .filter(|command| command.group == "custom")
            .collect();

        assert_eq!(custom.len(), 2);
        assert_eq!(custom[0].label, "open lazygit");
        assert_eq!(custom[0].shortcut, "prefix+alt+g");
        assert_eq!(custom[0].action, PaletteAction::Custom(0));
        assert_eq!(custom[1].label, "custom command");
        assert!(custom[1].config_key.is_none());
    }

    #[test]
    fn palette_compacts_multiple_indexed_ranges() {
        let config: crate::config::Config = toml::from_str(
            r#"
[keys]
switch_tab = ["prefix+1..9", "alt+1..9"]
switch_workspace = "ctrl+1..9"
"#,
        )
        .expect("config parses");

        let mut app = AppState::test_new();
        app.keybinds = config.keybinds();
        let commands = palette_commands(&app);

        let shortcut_for = |label: &str| {
            commands
                .iter()
                .find(|command| command.label == label)
                .map(|command| command.shortcut.clone())
                .unwrap_or_else(|| panic!("missing {label}"))
        };

        assert_eq!(shortcut_for("switch tab 1-9"), "prefix+1..9 / alt+1..9");
        assert_eq!(shortcut_for("switch workspace 1-9"), "ctrl+1..9");
    }

    #[test]
    fn palette_search_matches_command_names_and_shortcuts() {
        let app = AppState::test_new();
        let by_name = filter_palette_commands(palette_commands(&app), "split");
        assert!(by_name
            .iter()
            .all(|command| command.label.contains("split")));
        assert_eq!(by_name.len(), 2);

        let by_shortcut = filter_palette_commands(palette_commands(&app), "^prefix\\+t$");
        assert_eq!(by_shortcut.len(), 1);
        assert_eq!(by_shortcut[0].label, "new tab");
    }
}
