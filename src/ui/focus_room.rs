use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{focus_room::FocusRoomGeometry, AppState},
    terminal::TerminalRuntimeRegistry,
};

const FIND_LABEL: &str = " find agent ";
const OPS_LABEL: &str = " ops ⋯ ";
const RETURN_LABEL: &str = " ← focus ";

pub(crate) fn compute_geometry(
    area: Rect,
    dialog_open: bool,
    finder_open: bool,
    drawer_open: bool,
    return_button: bool,
) -> FocusRoomGeometry {
    let bar = Rect::new(area.x, area.y, area.width, u16::from(area.height > 0));
    let pane = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    let ops_width = super::text::display_width_u16(OPS_LABEL);
    let find_width = super::text::display_width_u16(FIND_LABEL);
    let ops_x = area.right().saturating_sub(1).saturating_sub(ops_width);
    let ops_hit_area = Rect::new(
        ops_x,
        area.y,
        ops_width.min(area.width),
        u16::from(area.height > 0),
    );
    let find_x = ops_x.saturating_sub(2).saturating_sub(find_width);
    let find_hit_area = Rect::new(
        find_x,
        area.y,
        find_width.min(area.right().saturating_sub(find_x)),
        u16::from(area.height > 0),
    );
    let return_width = super::text::display_width_u16(RETURN_LABEL).min(area.width);
    let return_hit_area = if return_button && area.height > 0 {
        Rect::new(area.x, area.y, return_width, 1)
    } else {
        Rect::default()
    };

    let dialog_width = 56.min(area.width);
    let dialog_height = 9.min(area.height.saturating_sub(1));
    let dialog_rect = if dialog_open && dialog_width > 0 && dialog_height > 0 {
        Rect::new(
            area.x
                .saturating_add(area.width.saturating_sub(dialog_width) / 2),
            area.y
                .saturating_add(1)
                .saturating_add(area.height.saturating_sub(1).saturating_sub(dialog_height) / 2),
            dialog_width,
            dialog_height,
        )
    } else {
        Rect::default()
    };
    let dialog_row_hit_areas = (0..3)
        .map(|index| {
            Rect::new(
                dialog_rect.x.saturating_add(1),
                dialog_rect.y.saturating_add(3 + index),
                dialog_rect.width.saturating_sub(2),
                u16::from(dialog_rect.height > 5),
            )
        })
        .collect();

    let finder_width = area.width.saturating_sub(8).min(114);
    let overlay_height = area.height.saturating_sub(4);
    let finder_spare_width = area.width.saturating_sub(finder_width);
    let finder_left_padding = finder_spare_width / 2 + u16::from(finder_spare_width > 8);
    let finder_rect = if finder_open && finder_width > 0 && overlay_height > 0 {
        Rect::new(
            area.x.saturating_add(finder_left_padding),
            area.y.saturating_add(2),
            finder_width,
            overlay_height,
        )
    } else {
        Rect::default()
    };

    let drawer_width = if area.width >= 140 {
        84
    } else {
        area.width.saturating_sub(10)
    }
    .min(area.width);
    let drawer_rect = if drawer_open && drawer_width > 0 && overlay_height > 0 {
        Rect::new(
            area.right().saturating_sub(2).saturating_sub(drawer_width),
            area.y.saturating_add(2),
            drawer_width,
            overlay_height,
        )
    } else {
        Rect::default()
    };

    FocusRoomGeometry {
        bar,
        pane,
        find_hit_area,
        ops_hit_area,
        return_hit_area,
        dialog_rect,
        finder_rect,
        drawer_rect,
        dialog_row_hit_areas,
    }
}

pub(crate) fn render(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    let geometry = &app.focus_room_ui.geometry;
    super::tab_surface::render_tab_surface(app, terminal_runtimes, app.view.tab_surface(), frame);
    render_focus_bar(app, terminal_runtimes, frame);

    if app.focus_room_ui.leave_dialog_open {
        super::dim_background(frame, frame.area());
        render_focus_bar(app, terminal_runtimes, frame);
        render_leave_dialog(app, frame, geometry.dialog_rect);
    }

    if app.focus_room_ui.drawer.is_some() && !app.focus_room_ui.leave_dialog_open {
        super::dim_background(frame, frame.area());
        render_drawer(app, frame, geometry.drawer_rect);
    } else if app.agent_finder_saved_query.is_some() && !app.focus_room_ui.leave_dialog_open {
        super::dim_background(frame, frame.area());
        render_finder(app, frame, geometry.finder_rect);
    }
    if app.capture.is_some() {
        super::capture::render(app, frame, frame.area());
    }
}

fn render_focus_bar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    let area = app.focus_room_ui.geometry.bar;
    if area.is_empty() {
        return;
    }
    let p = &app.palette;
    frame.render_widget(
        Paragraph::new(" ".repeat(usize::from(area.width))).style(Style::default().bg(p.panel_bg)),
        area,
    );
    let find = app.focus_room_ui.geometry.find_hit_area;
    let ops = app.focus_room_ui.geometry.ops_hit_area;
    let title = room_title(app, terminal_runtimes);
    let ack = app
        .capture_ack
        .as_ref()
        .map(|ack| format!("✓ {}", ack.text));
    let ack_width = ack
        .as_deref()
        .map(super::text::display_width_u16)
        .unwrap_or_default();
    let title_right = if ack_width > 0 {
        find.x.saturating_sub(4).saturating_sub(ack_width)
    } else {
        find.x.saturating_sub(1)
    };
    let title_start = area.x.saturating_add(1);
    let available = usize::from(title_right.saturating_sub(title_start));
    let prefix_width = usize::from(super::text::display_width_u16("focus · "));
    let compact_focus = title == "focus" || available <= prefix_width;
    let title_value = if compact_focus {
        super::text::truncate_end("focus", available)
    } else {
        super::text::truncate_end(&title, available.saturating_sub(prefix_width))
    };
    let title_width = if compact_focus {
        super::text::display_width_u16(&title_value)
    } else {
        super::text::display_width_u16("focus · ")
            .saturating_add(super::text::display_width_u16(&title_value))
    };
    let title_width = title_width
        .saturating_add(1)
        .min(title_right.saturating_sub(area.x));
    let mut title_line = vec![Span::styled(
        " ",
        Style::default().fg(p.text).bg(p.panel_bg),
    )];
    if compact_focus {
        title_line.push(Span::styled(
            title_value,
            Style::default()
                .fg(p.accent)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        title_line.extend([
            Span::styled(
                "focus",
                Style::default()
                    .fg(p.accent)
                    .bg(p.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(p.surface1).bg(p.panel_bg)),
            Span::styled(title_value, Style::default().fg(p.subtext0).bg(p.panel_bg)),
        ]);
    }
    frame.render_widget(
        Paragraph::new(Line::from(title_line)),
        Rect::new(area.x, area.y, title_width, 1),
    );
    if let Some(ack) = ack {
        let x = find.x.saturating_sub(4).saturating_sub(ack_width);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("✓", Style::default().fg(p.green).bg(p.panel_bg)),
                Span::styled(
                    ack.trim_start_matches('✓').to_owned(),
                    Style::default().fg(p.text).bg(p.panel_bg),
                ),
            ])),
            Rect::new(x, area.y, ack_width, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(FIND_LABEL).style(Style::default().fg(p.overlay1).bg(p.panel_bg)),
        find,
    );
    let ops_style = if app.focus_room_ui.leave_dialog_open {
        Style::default()
            .fg(p.panel_bg)
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay1).bg(p.panel_bg)
    };
    frame.render_widget(Paragraph::new(OPS_LABEL).style(ops_style), ops);
}

fn room_title(app: &AppState, terminal_runtimes: &TerminalRuntimeRegistry) -> String {
    let Some(ws_idx) = app.active else {
        return "focus".into();
    };
    let Some(workspace) = app.workspaces.get(ws_idx) else {
        return "focus".into();
    };
    let Some(pane_id) = workspace.focused_pane_id() else {
        return "focus".into();
    };
    let raw = app
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
        .and_then(|runtime| runtime.terminal_title())
        .or_else(|| {
            workspace
                .terminal_id(pane_id)
                .and_then(|id| app.terminals.get(id))
                .and_then(|terminal| terminal.terminal_title_stripped())
        })
        .unwrap_or_default();
    normalized_room_title(&raw)
}

fn normalized_room_title(raw: &str) -> String {
    let title = raw.trim();
    if title.is_empty() || title.eq_ignore_ascii_case("vellum") {
        return "focus".into();
    }
    if title.ends_with(".md") {
        return std::path::Path::new(title)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| "focus".into());
    }
    title.to_owned()
}

fn render_leave_dialog(app: &AppState, frame: &mut Frame, rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let Some(inner) =
        super::widgets::render_panel_shell(frame, rect, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let p = &app.palette;
    let title = Rect::new(
        inner.x.saturating_add(1),
        inner.y,
        inner.width.saturating_sub(1),
        1,
    );
    frame.render_widget(
        Paragraph::new("leave focus?").style(
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
        title,
    );
    let options = [
        ("c", "capture an idea", "stays in focus"),
        ("f", "find one agent", "stays in focus"),
        ("o", "open operations", ""),
    ];
    for (index, (key, label, note)) in options.iter().enumerate() {
        let y = inner.y.saturating_add(2 + index as u16);
        if y >= inner.bottom() {
            continue;
        }
        let selected = app.focus_room_ui.leave_selection == index;
        let bg = if selected { p.selection_bg } else { p.panel_bg };
        let bold = if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        let row = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(Paragraph::new("").style(Style::default().bg(bg)), row);
        let text_x = inner.x.saturating_add(1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{key}  "),
                    Style::default()
                        .fg(p.accent)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    label.to_string(),
                    Style::default().fg(p.text).bg(bg).add_modifier(bold),
                ),
            ])),
            Rect::new(text_x, y, inner.width.saturating_sub(1), 1),
        );
        if !note.is_empty() {
            let note_width = u16::try_from(note.chars().count()).unwrap_or(u16::MAX);
            let note_x = inner.right().saturating_sub(1).saturating_sub(note_width);
            frame.render_widget(
                Paragraph::new(*note).style(Style::default().fg(p.overlay0).bg(bg)),
                Rect::new(note_x, y, note_width, 1),
            );
        }
    }
    if inner.height >= 6 {
        frame.render_widget(
            Paragraph::new("ends this focus block · nothing is discarded")
                .style(Style::default().fg(p.overlay1).bg(p.panel_bg)),
            Rect::new(
                inner.x.saturating_add(4),
                inner.y.saturating_add(5),
                inner.width.saturating_sub(4),
                1,
            ),
        );
    }
    if inner.height >= 7 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "esc",
                    Style::default()
                        .fg(p.accent)
                        .bg(p.panel_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " stay in focus",
                    Style::default().fg(p.overlay0).bg(p.panel_bg),
                ),
            ])),
            Rect::new(
                inner.x.saturating_add(1),
                inner.bottom().saturating_sub(1),
                inner.width.saturating_sub(1),
                1,
            ),
        );
    }
}

fn render_finder(app: &AppState, frame: &mut Frame, rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let Some(_inner) =
        super::widgets::render_panel_shell(frame, rect, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let inner = Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );
    if inner.is_empty() {
        return;
    }
    let p = &app.palette;
    let query = app.sidebar_work_filter.query.as_str();
    let head = if query.is_empty() {
        "⌕ ▌".to_owned()
    } else {
        format!("⌕ {query}▌")
    };
    let close_width = 7.min(inner.width);
    let close_x = inner.right().saturating_sub(close_width);
    frame.render_widget(
        Paragraph::new(super::text::truncate_end(
            &head,
            usize::from(close_x.saturating_sub(inner.x)),
        ))
        .style(
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(inner.x, inner.y, close_x.saturating_sub(inner.x), 1),
    );
    frame.render_widget(
        Paragraph::new(" close").style(Style::default().fg(p.overlay0).bg(p.panel_bg)),
        Rect::new(close_x, inner.y, close_width, 1),
    );
    draw_rule(
        frame,
        Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1),
        p,
    );
    let footer_height = 2.min(inner.height.saturating_sub(2));
    let body_height = inner.height.saturating_sub(2 + footer_height);
    let results_height = body_height.saturating_add(1) / 2;
    let results = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        results_height,
    );
    super::sidebar::render_agent_finder_results_in_area(app, frame, results);
    let divider_y = results.bottom();
    if divider_y < inner.bottom().saturating_sub(footer_height) {
        draw_rule(frame, Rect::new(inner.x, divider_y, inner.width, 1), p);
    }
    let preview_y = divider_y.saturating_add(1);
    let footer_y = inner.bottom().saturating_sub(footer_height);
    if preview_y < footer_y {
        super::render_agent_finder_preview(
            app,
            frame,
            Rect::new(
                inner.x,
                preview_y,
                inner.width,
                footer_y.saturating_sub(preview_y),
            ),
        );
    }
    let selection = if app.agent_finder_results.is_empty() {
        "0 of 0".to_owned()
    } else {
        format!(
            "{} of {}",
            app.agent_finder_selected
                .min(app.agent_finder_results.len().saturating_sub(1))
                .saturating_add(1),
            app.agent_finder_results.len()
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("{selection} · ↑/↓ results · ctrl+↑/↓ history"),
                Style::default().fg(p.overlay0).bg(p.panel_bg),
            )),
            Line::from(Span::styled(
                "⏎ open in drawer · esc back",
                Style::default().fg(p.overlay0).bg(p.panel_bg),
            )),
        ]),
        Rect::new(inner.x, footer_y, inner.width, footer_height),
    );
}

fn render_drawer(app: &AppState, frame: &mut Frame, rect: Rect) {
    let Some(drawer) = app.focus_room_ui.drawer.as_ref() else {
        return;
    };
    if rect.is_empty() {
        return;
    }
    let Some(_inner) =
        super::widgets::render_panel_shell(frame, rect, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let inner = Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );
    if inner.is_empty() {
        return;
    }
    let p = &app.palette;
    let title = format!(
        " {} · {} · {} ",
        drawer.title, drawer.agent_label, drawer.host
    );
    frame.render_widget(
        Paragraph::new(super::text::truncate_end(
            &title,
            usize::from(rect.width.saturating_sub(2)),
        ))
        .style(
            Style::default()
                .fg(p.accent)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(
            rect.x.saturating_add(1),
            rect.y,
            rect.width.saturating_sub(2),
            1,
        ),
    );
    frame.render_widget(
        Paragraph::new("⋯ earlier output in operations")
            .style(Style::default().fg(p.overlay0).bg(p.panel_bg)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if inner.height < 5 {
        return;
    }
    let divider_y = inner.bottom().saturating_sub(3);
    let content_y = inner.y.saturating_add(2);
    let content_height = divider_y.saturating_sub(content_y);
    let mut wrapped = drawer
        .lines
        .iter()
        .flat_map(|line| wrap_line(line, inner.width as usize))
        .collect::<Vec<_>>();
    if wrapped.len() > usize::from(content_height) {
        wrapped.drain(..wrapped.len() - usize::from(content_height));
    }
    let start_y = divider_y.saturating_sub(wrapped.len() as u16);
    for (index, line) in wrapped.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(line.as_str()).style(Style::default().fg(p.text).bg(p.panel_bg)),
            Rect::new(
                inner.x,
                start_y.saturating_add(index as u16),
                inner.width,
                1,
            ),
        );
    }
    draw_rule(frame, Rect::new(inner.x, divider_y, inner.width, 1), p);
    let input_y = divider_y.saturating_add(1);
    if drawer.remote {
        frame.render_widget(
            Paragraph::new(format!("on {} · send from operations", drawer.host))
                .style(Style::default().fg(p.overlay0).bg(p.panel_bg)),
            Rect::new(inner.x, input_y, inner.width, 1),
        );
    } else {
        let before_cursor = drawer.input.chars().take(drawer.cursor).collect::<String>();
        let cursor_x = inner
            .x
            .saturating_add(2)
            .saturating_add(super::text::display_width_u16(&before_cursor))
            .min(inner.right().saturating_sub(1));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(p.overlay1).bg(p.panel_bg)),
                Span::styled(
                    drawer.input.clone(),
                    Style::default().fg(p.text).bg(p.panel_bg),
                ),
                Span::styled("█", Style::default().fg(p.accent).bg(p.panel_bg)),
            ])),
            Rect::new(inner.x, input_y, inner.width, 1),
        );
        frame.set_cursor_position((cursor_x, input_y));
    }
    let footer_y = input_y.saturating_add(1);
    let right_text = "last lines only";
    let right_width = super::text::display_width_u16(right_text).min(inner.width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "↵",
                Style::default()
                    .fg(p.accent)
                    .bg(p.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" send  ", Style::default().fg(p.overlay0).bg(p.panel_bg)),
            Span::styled(
                "q",
                Style::default()
                    .fg(p.accent)
                    .bg(p.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " back to focus",
                Style::default().fg(p.overlay0).bg(p.panel_bg),
            ),
        ])),
        Rect::new(
            inner.x,
            footer_y,
            inner.width.saturating_sub(right_width),
            1,
        ),
    );
    frame.render_widget(
        Paragraph::new(right_text).style(Style::default().fg(p.overlay0).bg(p.panel_bg)),
        Rect::new(
            inner.right().saturating_sub(right_width),
            footer_y,
            right_width,
            1,
        ),
    );
}

fn draw_rule(frame: &mut Frame, area: Rect, palette: &crate::app::state::Palette) {
    if area.width > 0 && area.height > 0 {
        frame.render_widget(
            Paragraph::new("─".repeat(usize::from(area.width)))
                .style(Style::default().fg(palette.overlay0).bg(palette.panel_bg)),
            area,
        );
    }
}

fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0usize;
    for character in line.chars() {
        let char_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + char_width > width && !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push(character);
        used += char_width;
    }
    rows.push(row);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn room_overlay_geometry_matches_wide_and_narrow_screens() {
        let wide = compute_geometry(Rect::new(0, 0, 160, 45), false, true, false, false);
        assert_eq!(wide.pane, Rect::new(0, 1, 160, 44));
        assert_eq!(wide.finder_rect, Rect::new(24, 2, 114, 41));
        let narrow = compute_geometry(Rect::new(0, 0, 100, 30), false, true, true, false);
        assert_eq!(narrow.finder_rect, Rect::new(4, 2, 92, 26));
        assert_eq!(narrow.drawer_rect, Rect::new(8, 2, 90, 26));
        assert_eq!(narrow.dialog_rect, Rect::default());
        let dialog = compute_geometry(Rect::new(0, 0, 160, 45), true, false, false, false);
        assert_eq!(dialog.dialog_rect, Rect::new(52, 18, 56, 9));
        let narrow_dialog = compute_geometry(Rect::new(0, 0, 100, 30), true, false, false, false);
        assert_eq!(narrow_dialog.dialog_rect, Rect::new(22, 11, 56, 9));
    }

    #[test]
    fn room_title_falls_back_and_uses_markdown_file_stems() {
        assert_eq!(normalized_room_title(""), "focus");
        assert_eq!(normalized_room_title("vellum"), "focus");
        assert_eq!(normalized_room_title("notes/plan.md"), "plan");
        assert_eq!(normalized_room_title("planning"), "planning");
        assert_eq!(
            super::super::text::truncate_end("a very long note title", 10),
            "a very lo…"
        );
    }

    #[test]
    fn focus_bar_renders_exact_spans_and_keeps_the_right_edge_clear() {
        let mut app = AppState::test_new();
        app.focus_room_ui.geometry =
            compute_geometry(Rect::new(0, 0, 160, 45), false, false, false, false);
        let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
        terminal
            .draw(|frame| render_focus_bar(&app, &TerminalRuntimeRegistry::new(), frame))
            .unwrap();
        let row = (0..160)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        assert_eq!(
            row,
            format!(" focus{} find agent    ops ⋯  ", " ".repeat(132))
        );
        assert_eq!(
            terminal.backend().buffer()[(0, 0)].style().bg,
            Some(app.palette.panel_bg)
        );
    }

    #[test]
    fn focus_bar_title_stops_before_the_find_button_on_narrow_screens() {
        let mut app = AppState::test_new();
        app.focus_room_ui.geometry =
            compute_geometry(Rect::new(0, 0, 28, 20), false, false, false, false);
        let mut terminal = Terminal::new(TestBackend::new(28, 20)).unwrap();
        terminal
            .draw(|frame| render_focus_bar(&app, &TerminalRuntimeRegistry::new(), frame))
            .unwrap();
        let row = (0..28)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();

        assert!(row.starts_with(" foc…  find agent"), "{row:?}");
        assert_eq!(app.focus_room_ui.geometry.find_hit_area.x, 6);
        assert_eq!(terminal.backend().buffer()[(5, 0)].symbol(), " ");
    }

    #[test]
    fn leave_dialog_draws_its_title_and_selected_open_row() {
        let mut app = AppState::test_new();
        app.focus_room_ui.leave_selection = 2;
        let rect = Rect::new(52, 18, 56, 9);
        let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
        terminal
            .draw(|frame| render_leave_dialog(&app, frame, rect))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(54, 19)].symbol(), "l");
        assert_eq!(buffer[(57, 23)].symbol(), "o");
        assert_eq!(buffer[(57, 23)].style().bg, Some(app.palette.selection_bg));
        assert_eq!(buffer[(53, 23)].style().bg, Some(app.palette.selection_bg));
        assert_eq!(buffer[(106, 23)].style().bg, Some(app.palette.selection_bg));
        assert_eq!(buffer[(105, 21)].symbol(), "s");
        assert_eq!(buffer[(102, 21)].symbol(), "o");
        assert_eq!(buffer[(57, 24)].symbol(), "e");
    }

    #[test]
    fn focus_room_pane_surface_uses_the_entire_body_without_chrome() {
        let mut app = AppState::test_new();
        let workspace = crate::workspace::Workspace::test_new("focus");
        let tab_id =
            crate::workspace::public_tab_id_for_number(&workspace.id, workspace.tabs[0].number);
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.focus_room.tab_id = Some(tab_id);
        app.focus_room.active = true;
        let area = Rect::new(0, 1, 160, 44);
        let surface = super::super::tab_surface::compute_focus_room_surface(
            &app,
            &TerminalRuntimeRegistry::new(),
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        assert_eq!(surface.pane_infos.len(), 1);
        assert_eq!(surface.pane_infos[0].rect, area);
        assert_eq!(surface.pane_infos[0].inner_rect, area);
        assert!(surface.pane_infos[0].borders.is_empty());
        assert!(surface.split_borders.is_empty());
    }
}
