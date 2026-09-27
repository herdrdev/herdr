//! The sidebar notepad panel: a header naming the active note and the editable
//! note body under it, pinned to the bottom of the sidebar above the footer
//! icons.
//!
//! Geometry is pure so the panel's rows can be reserved before anything draws;
//! [`super::sidebar::workspace_list_rect_for_app`] subtracts them from the
//! workspace list.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::text::{display_width_u16, truncate_end};
use crate::app::state::Palette;
use crate::app::AppState;

/// Header row plus at least one body row.
const MIN_NOTEPAD_ROWS: u16 = 2;

/// How many rows of `content` the notepad panel occupies. Zero whenever it is
/// off, the sidebar is collapsed, or the list would be squeezed below its
/// visible header and three-row content floor.
pub(crate) fn notepad_height(app: &AppState, content: Rect) -> u16 {
    if !app.notepad.enabled || app.sidebar_collapsed || content.width == 0 {
        return 0;
    }
    let spare = content
        .height
        .saturating_sub(super::sidebar::sidebar_workspace_floor(app));
    if spare < MIN_NOTEPAD_ROWS {
        return 0;
    }
    app.notepad.height.min(spare)
}

/// The panel itself, at the bottom of the sidebar's content area.
pub(crate) fn notepad_panel_rect(app: &AppState, content: Rect) -> Rect {
    let height = notepad_height(app, content);
    if height == 0 {
        return Rect::default();
    }
    Rect::new(
        content.x,
        content.bottom().saturating_sub(height),
        content.width,
        height,
    )
}

/// The note body rows inside the panel, header excluded.
pub(crate) fn notepad_body_rect(panel: Rect) -> Rect {
    if panel.height <= 1 {
        return Rect::default();
    }
    Rect::new(
        panel.x,
        panel.y.saturating_add(1),
        panel.width,
        panel.height.saturating_sub(1),
    )
}

fn read_only_tab_labels(width: u16) -> (&'static str, &'static str, &'static str) {
    if width >= 32 {
        (
            crate::notepad::NOTEPAD_CONTEXT_TAB_LABEL,
            super::notepad_agent::TAB_LABEL,
            super::notepad_usage::TAB_LABEL,
        )
    } else {
        ("c", "a", "u")
    }
}

/// Clickable name segments in the header, one per note, left to right, then
/// the Context tab and the agent tab after their divider. Only the names that
/// fit are returned, so a click can never resolve to a tab the operator cannot
/// see.
pub(crate) fn notepad_tab_hit_areas(
    app: &AppState,
    panel: Rect,
) -> Vec<(crate::notepad::NotepadTabTarget, Rect)> {
    if panel.width == 0 || panel.height == 0 {
        return Vec::new();
    }
    let mut areas = Vec::new();
    let mut x = panel.x.saturating_add(2);
    let right = panel.right();
    let (context_label, agent_label, usage_label) = read_only_tab_labels(panel.width);
    let context_width = display_width_u16(context_label);
    let agent_width = display_width_u16(agent_label);
    let usage_width = display_width_u16(usage_label);
    let pane_tabs_width = 2u16
        .saturating_add(context_width)
        .saturating_add(1)
        .saturating_add(agent_width)
        .saturating_add(1)
        .saturating_add(usage_width);
    for (index, file) in app.notepad.files.iter().enumerate() {
        let width = display_width_u16(&file.name);
        let next_x = x.saturating_add(width).saturating_add(1);
        if width == 0 || next_x.saturating_add(pane_tabs_width) > right {
            break;
        }
        areas.push((
            crate::notepad::NotepadTabTarget::Note(index),
            Rect::new(x, panel.y, width, 1),
        ));
        x = next_x;
    }
    if app.notepad.files.is_empty() {
        x = x.saturating_add(display_width_u16("no notes"));
    }
    let context_x = x.saturating_add(2);
    if context_x.saturating_add(context_width) <= right {
        areas.push((
            crate::notepad::NotepadTabTarget::Context,
            Rect::new(context_x, panel.y, context_width, 1),
        ));
        let agent_x = context_x.saturating_add(context_width).saturating_add(1);
        if agent_x.saturating_add(agent_width) <= right {
            areas.push((
                crate::notepad::NotepadTabTarget::Agent,
                Rect::new(agent_x, panel.y, agent_width, 1),
            ));
            let usage_x = agent_x.saturating_add(agent_width).saturating_add(1);
            if usage_x.saturating_add(usage_width) <= right {
                areas.push((
                    crate::notepad::NotepadTabTarget::Usage,
                    Rect::new(usage_x, panel.y, usage_width, 1),
                ));
            }
        }
    }
    areas
}

fn tab_style(palette: &Palette, active: bool, focused: bool) -> Style {
    if active && focused {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else if active {
        Style::default().fg(palette.text)
    } else {
        Style::default().fg(palette.overlay0)
    }
}

fn header_spans<'a>(app: &'a AppState, palette: &Palette, width: u16) -> Line<'a> {
    let focused = app.notepad.focused;
    let mut spans = vec![Span::styled(
        "▤ ",
        Style::default().fg(if focused {
            palette.accent
        } else {
            palette.overlay0
        }),
    )];
    let mut used = 2u16;
    let (context_label, agent_label, usage_label) = read_only_tab_labels(width);
    let context_width = display_width_u16(context_label);
    let agent_width = display_width_u16(agent_label);
    let usage_width = display_width_u16(usage_label);
    let pane_tabs_width = 2u16
        .saturating_add(context_width)
        .saturating_add(1)
        .saturating_add(agent_width)
        .saturating_add(1)
        .saturating_add(usage_width);
    for (index, file) in app.notepad.files.iter().enumerate() {
        let name_width = display_width_u16(&file.name);
        let next_used = used.saturating_add(name_width).saturating_add(1);
        if next_used.saturating_add(pane_tabs_width) > width {
            break;
        }
        let active = !app.notepad.context_active
            && !app.notepad.agent_tab
            && !app.notepad.usage_tab
            && index == app.notepad.active;
        let style = tab_style(palette, active, focused);
        spans.push(Span::styled(file.name.as_str(), style));
        spans.push(Span::raw(" "));
        used = next_used;
    }
    if app.notepad.files.is_empty() {
        spans.push(Span::styled(
            "no notes",
            Style::default().fg(palette.overlay0),
        ));
        used = used.saturating_add(display_width_u16("no notes"));
    }
    // The two tabs about the focused pane trail the note names, divided by a
    // pipe: Context hosts its work context, agent its server-owned state.
    if used.saturating_add(2).saturating_add(context_width) <= width {
        spans.push(Span::styled("│", Style::default().fg(palette.surface_dim)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            context_label,
            tab_style(palette, app.notepad.context_active, focused),
        ));
        used = used.saturating_add(2).saturating_add(context_width);
        if used.saturating_add(1).saturating_add(agent_width) <= width {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                agent_label,
                tab_style(palette, app.notepad.agent_tab, focused),
            ));
            used = used.saturating_add(1).saturating_add(agent_width);
            if used.saturating_add(1).saturating_add(usage_width) <= width {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    usage_label,
                    tab_style(palette, app.notepad.usage_tab, focused),
                ));
                used = used.saturating_add(1).saturating_add(usage_width);
            }
        }
    }
    // Unsaved and diverged are the two facts the operator cannot recover by
    // looking at the body, so they get the remaining space.
    let marker = if app.notepad.error.is_some() {
        Some(("!", palette.red))
    } else if app.notepad.diverged {
        Some(("⇅", palette.peach))
    } else if app.notepad.dirty {
        Some(("•", palette.yellow))
    } else {
        None
    };
    if let Some((glyph, color)) = marker {
        if used < width {
            spans.push(Span::styled(glyph, Style::default().fg(color)));
        }
    }
    Line::from(spans)
}

pub(crate) fn render_notepad(app: &AppState, frame: &mut Frame, panel: Rect) {
    if panel.width == 0 || panel.height == 0 {
        return;
    }
    let palette = &app.palette;
    let separator = Style::default().fg(palette.surface_dim);
    {
        let buf = frame.buffer_mut();
        for x in panel.x..panel.right() {
            buf[(x, panel.y)].set_symbol("─");
            buf[(x, panel.y)].set_style(separator);
        }
    }
    frame.render_widget(
        Paragraph::new(header_spans(app, palette, panel.width)),
        Rect::new(panel.x, panel.y, panel.width, 1),
    );

    let body = notepad_body_rect(panel);
    if body.height == 0 {
        return;
    }
    if app.notepad.agent_tab {
        super::notepad_agent::render_agent_body(app, frame, body);
        return;
    }
    if app.notepad.usage_tab {
        super::notepad_usage::render_usage_body(app, frame, body);
        return;
    }
    if let Some(error) = &app.notepad.error {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(error, usize::from(body.width)),
                Style::default().fg(palette.red),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }
    if app.notepad.context_active {
        crate::ui::dock_context::render_context(app, frame, body);
        return;
    }

    let text_style = Style::default().fg(palette.text);
    let empty_style = Style::default().fg(palette.overlay0);
    let visible = usize::from(body.height);
    // The hint is for an untouched note; once the caret is in it, it is noise.
    let placeholder = !app.notepad.focused && app.notepad.lines.iter().all(String::is_empty);
    for row in 0..visible {
        let line_index = app.notepad.scroll + row;
        let y = body.y.saturating_add(row as u16);
        if placeholder && row == 0 {
            frame.render_widget(
                Paragraph::new(Span::styled("type a note…", empty_style)),
                Rect::new(body.x, y, body.width, 1),
            );
            continue;
        }
        let Some(line) = app.notepad.lines.get(line_index) else {
            break;
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(line, usize::from(body.width)),
                text_style,
            )),
            Rect::new(body.x, y, body.width, 1),
        );
    }
}

/// Where the note's caret belongs on screen, or `None` when the notepad does
/// not hold it. The caret carries the IME composition preview, so it has to be
/// a real host cursor rather than a highlighted cell.
pub(crate) fn notepad_caret_position(app: &AppState, panel: Rect) -> Option<(u16, u16)> {
    if !app.notepad.focused
        || app.notepad.agent_tab
        || app.notepad.usage_tab
        || app.notepad.context_active
        || app.notepad.error.is_some()
    {
        return None;
    }
    let body = notepad_body_rect(panel);
    if body.width == 0 || body.height == 0 {
        return None;
    }
    let row = app.notepad.cursor_line.saturating_sub(app.notepad.scroll);
    if row >= usize::from(body.height) {
        return None;
    }
    let prefix: String = app
        .notepad
        .lines
        .get(app.notepad.cursor_line)
        .map(|line| line.chars().take(app.notepad.cursor_col).collect())
        .unwrap_or_default();
    let x = body
        .x
        .saturating_add(display_width_u16(&prefix))
        .min(body.right().saturating_sub(1));
    Some((x, body.y.saturating_add(row as u16)))
}

/// Place the caret after every other surface has drawn. The focused pane, the
/// home composer and the sidebar all claim the one host cursor, and the last
/// claim wins; the notepad takes keys ahead of all of them, so it has to be the
/// one that speaks last. Only the break prompt, which takes keys ahead of the
/// notepad, may still overwrite it.
pub(crate) fn render_notepad_caret(app: &AppState, frame: &mut Frame) {
    if let Some(position) = notepad_caret_position(app, app.view.notepad_rect) {
        frame.set_cursor_position(position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::notepad::NotepadTabTarget;

    fn state() -> AppState {
        let mut app = AppState::test_new();
        app.notepad.enabled = true;
        app.notepad.height = 8;
        app
    }

    #[test]
    fn a_disabled_notepad_reserves_no_rows() {
        let mut app = state();
        app.notepad.enabled = false;
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 40)), 0);
    }

    #[test]
    fn the_panel_never_squeezes_the_workspace_list_below_its_floor() {
        let app = state();
        // The list keeps its three-row floor; the panel takes the rest, up to
        // its own configured height.
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 8)), 5);
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 13)), 8);
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 40)), 8);
        // Below the floor plus one usable panel row there is nothing to give.
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 4)), 0);
    }

    #[test]
    fn a_tall_panel_is_limited_only_by_the_rows_the_sidebar_can_spare() {
        let mut app = state();
        // Past the old fixed ceiling of 24 rows.
        app.notepad.height = 60;
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 80)), 60);
        // And the sidebar's spare rows still win when the panel wants more.
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 40)), 37);
    }

    #[test]
    fn the_panel_sits_at_the_bottom_of_the_content_area() {
        let app = state();
        let content = Rect::new(0, 1, 26, 40);
        let panel = notepad_panel_rect(&app, content);
        assert_eq!(panel, Rect::new(0, 33, 26, 8));
        assert_eq!(notepad_body_rect(panel), Rect::new(0, 34, 26, 7));
    }

    #[test]
    fn header_hit_areas_stop_at_the_panel_edge() {
        let mut app = state();
        app.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/ideas.md".into(),
                name: "ideas".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/verylongnotename.md".into(),
                name: "verylongnotename".into(),
            },
        ]);
        let areas = notepad_tab_hit_areas(&app, Rect::new(0, 10, 26, 8));
        assert_eq!(
            areas,
            vec![
                (NotepadTabTarget::Note(0), Rect::new(2, 10, 4, 1)),
                (NotepadTabTarget::Note(1), Rect::new(7, 10, 5, 1)),
                (NotepadTabTarget::Context, Rect::new(15, 10, 1, 1)),
                (NotepadTabTarget::Agent, Rect::new(17, 10, 1, 1)),
                (NotepadTabTarget::Usage, Rect::new(19, 10, 1, 1)),
            ]
        );
        // The later notes yield because the read-only tabs own the suffix.
    }

    #[test]
    fn the_pane_tabs_follow_the_last_note_with_a_divider() {
        let mut app = state();
        app.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/todo.md".into(),
            name: "todo".into(),
        }]);
        let areas = notepad_tab_hit_areas(&app, Rect::new(0, 10, 26, 8));
        assert_eq!(
            areas,
            vec![
                (NotepadTabTarget::Note(0), Rect::new(2, 10, 4, 1)),
                (NotepadTabTarget::Context, Rect::new(9, 10, 1, 1)),
                (NotepadTabTarget::Agent, Rect::new(11, 10, 1, 1)),
                (NotepadTabTarget::Usage, Rect::new(13, 10, 1, 1)),
            ]
        );
    }

    #[test]
    fn note_tabs_yield_to_the_pane_tabs_at_the_minimum_sidebar_width() {
        let mut app = state();
        app.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/ideas.md".into(),
                name: "ideas".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
        ]);

        let areas = notepad_tab_hit_areas(&app, Rect::new(0, 10, 18, 8));
        assert_eq!(
            areas.last().map(|(target, _)| *target),
            Some(NotepadTabTarget::Usage),
            "the read-only tab stays reachable after note tabs yield"
        );
        assert_eq!(
            areas,
            vec![
                (NotepadTabTarget::Note(0), Rect::new(2, 10, 5, 1)),
                (NotepadTabTarget::Context, Rect::new(10, 10, 1, 1)),
                (NotepadTabTarget::Agent, Rect::new(12, 10, 1, 1)),
                (NotepadTabTarget::Usage, Rect::new(14, 10, 1, 1)),
            ]
        );
    }

    #[test]
    fn empty_notes_pane_tab_hit_areas_match_the_rendered_label_columns() {
        let app = state();
        let panel = Rect::new(0, 10, 32, 8);
        let areas = notepad_tab_hit_areas(&app, panel);
        assert_eq!(
            areas,
            vec![
                (NotepadTabTarget::Context, Rect::new(12, 10, 7, 1)),
                (NotepadTabTarget::Agent, Rect::new(20, 10, 5, 1)),
                (NotepadTabTarget::Usage, Rect::new(26, 10, 5, 1)),
            ]
        );
        let header = header_spans(&app, &app.palette, panel.width)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(
            header.chars().skip(12).take(7).collect::<String>(),
            "context"
        );
        assert_eq!(header.chars().skip(20).take(5).collect::<String>(), "agent");
        assert_eq!(header.chars().skip(26).take(5).collect::<String>(), "usage");
    }

    #[test]
    fn tab_hit_areas_never_extend_past_the_visible_header() {
        let mut app = state();
        app.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/a-very-long-note.md".into(),
            name: "a-very-long-note".into(),
        }]);
        for width in [1, 8, 12, 18, 26, 32] {
            let panel = Rect::new(4, 10, width, 8);
            let areas = notepad_tab_hit_areas(&app, panel);
            assert!(
                areas.iter().all(|(_, area)| {
                    area.x >= panel.x && area.right() <= panel.right() && area.width > 0
                }),
                "width {width}: {areas:?}"
            );
            if width >= 18 {
                let header = header_spans(&app, &app.palette, width)
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                assert!(display_width_u16(&header) <= width, "width {width}");
                let labels = if width >= 32 {
                    "│ context agent usage"
                } else {
                    "│ c a u"
                };
                assert!(header.contains(labels), "width {width}: {header}");
            }
        }
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::notepad::NotepadTabTarget;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_text(terminal: &Terminal<TestBackend>) -> Vec<String> {
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(terminal.backend().buffer().area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    #[test]
    fn read_only_tabs_render_lowercase_fallback_and_full_labels() {
        for width in [18, 26, 32] {
            let mut app = AppState::test_new();
            app.notepad.enabled = true;
            app.notepad.set_files(vec![
                crate::notepad::NotepadFile {
                    path: "/notes/ideas.md".into(),
                    name: "ideas".into(),
                },
                crate::notepad::NotepadFile {
                    path: "/notes/todo.md".into(),
                    name: "todo".into(),
                },
            ]);
            let panel = Rect::new(0, 0, width, 2);
            let mut terminal = Terminal::new(TestBackend::new(width, 2)).expect("test terminal");
            terminal
                .draw(|frame| render_notepad(&app, frame, panel))
                .expect("render");
            let header = buffer_text(&terminal)[0].clone();
            let labels = if width >= 32 {
                "│ context agent usage"
            } else {
                "│ c a u"
            };
            assert!(header.contains(labels), "width {width}: {header}");
            assert_eq!(
                notepad_tab_hit_areas(&app, panel)
                    .last()
                    .map(|(target, _)| *target),
                Some(NotepadTabTarget::Usage)
            );
        }
    }

    /// The panel has to reach a real frame, not just its geometry helpers: a
    /// note the operator cannot see is not a notepad.
    #[test]
    fn the_panel_draws_its_note_name_and_body_into_the_frame() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = AppState::test_new();
        app.notepad.enabled = true;
        app.notepad.height = 6;
        app.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/ideas.md".into(),
                name: "ideas".into(),
            },
        ]);
        app.notepad.set_body("- ship the notepad\n- take a break");

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let sidebar = app.view.sidebar_rect;
        assert!(sidebar.width > 0, "the wide layout keeps a sidebar");
        assert_eq!(app.view.notepad_rect.height, 6);
        assert_eq!(app.view.notepad_rect.bottom(), sidebar.bottom() - 1);
        assert_eq!(app.view.notepad_tab_hit_areas.len(), 5);
        assert_eq!(
            app.view
                .notepad_tab_hit_areas
                .last()
                .map(|(target, _)| *target),
            Some(crate::notepad::NotepadTabTarget::Usage)
        );

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let rows = buffer_text(&terminal);
        let panel_rows = &rows
            [usize::from(app.view.notepad_rect.y)..usize::from(app.view.notepad_rect.bottom())];
        assert!(
            panel_rows[0].contains("todo") && panel_rows[0].contains("│ c a u"),
            "header lists the notes that fit and compact read-only tabs: {:?}",
            panel_rows[0]
        );
        assert!(
            panel_rows
                .iter()
                .any(|row| row.contains("- ship the notepad")),
            "body is drawn: {panel_rows:?}"
        );
    }

    /// The Context tab hosts the dock's work-context surface in the panel:
    /// the header offers the tab, and the body draws the focused pane's
    /// context with its link rows registered for the click-to-copy path.
    #[test]
    fn the_context_tab_renders_work_context_with_hittable_link_rows() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("review-space")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        {
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.manual_label = Some("focused worker".into());
            terminal
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    ticket_ids: Some(vec!["MAT-128".into()]),
                    ..Default::default()
                })
                .expect("work context");
        }
        app.notepad.enabled = true;
        app.notepad.height = 12;
        app.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/todo.md".into(),
            name: "todo".into(),
        }]);
        app.notepad.context_active = true;

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let panel = app.view.notepad_rect;
        assert!(panel.height > 0, "the panel is up");
        assert!(app
            .view
            .notepad_tab_hit_areas
            .iter()
            .any(|(target, _)| *target == crate::notepad::NotepadTabTarget::Context));
        assert!(
            app.view
                .work_context_link_rows
                .iter()
                .any(|row| row.rect.y >= panel.y),
            "the tab's link rows register against the panel"
        );
        // The read-only tab never claims the host caret.
        assert!(notepad_caret_position(&app, panel).is_none());

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let rows = buffer_text(&terminal);
        let panel_rows = &rows[usize::from(panel.y)..usize::from(panel.bottom())];
        assert!(
            panel_rows[0].contains("│ c a u"),
            "header carries the Context tab: {:?}",
            panel_rows[0]
        );
        assert!(
            panel_rows.iter().any(|row| row.contains("CONTEXT")),
            "body renders the context surface: {panel_rows:?}"
        );
        assert!(
            panel_rows.iter().any(|row| row.contains("focused"))
                && panel_rows.iter().any(|row| row.contains("review-space")),
            "body renders the focused pane's fields: {panel_rows:?}"
        );
        assert!(
            panel_rows.iter().any(|row| row.contains("MAT-128")),
            "body renders the work link: {panel_rows:?}"
        );
    }

    #[test]
    fn the_workspace_list_gives_up_exactly_the_rows_the_panel_takes() {
        let mut app = AppState::test_new();
        let sidebar = Rect::new(0, 1, 26, 30);
        let without = crate::ui::workspace_list_rect_for_app(&app, sidebar);
        app.notepad.enabled = true;
        app.notepad.height = 8;
        let with = crate::ui::workspace_list_rect_for_app(&app, sidebar);
        assert_eq!(without.height - with.height, 8);
        assert_eq!(with.y, without.y);
    }
    /// The focused pane, the home composer and the notepad all claim the one
    /// host cursor, and the last claim of the frame wins. The notepad takes
    /// keys ahead of both, so it has to win.
    #[test]
    fn the_notepad_caret_outranks_every_other_cursor_claim() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("alpha")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.notepad.enabled = true;
        app.notepad.height = 6;
        app.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/todo.md".into(),
            name: "todo".into(),
        }]);
        app.notepad.set_body("- ship the caret");
        app.notepad.focused = true;
        app.notepad.cursor_line = 0;
        app.notepad.cursor_col = "- ship".chars().count();

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let expected = notepad_caret_position(&app, app.view.notepad_rect).expect("caret");
        let body = notepad_body_rect(app.view.notepad_rect);
        assert_eq!(expected, (body.x + 6, body.y));

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let caret = terminal.get_cursor_position().expect("host cursor");
        assert_eq!((caret.x, caret.y), expected);
    }

    #[test]
    fn an_unfocused_or_unreachable_notepad_claims_no_caret() {
        let mut app = AppState::test_new();
        app.notepad.enabled = true;
        app.notepad.height = 8;
        app.notepad.set_body("note");
        let panel = notepad_panel_rect(&app, Rect::new(0, 1, 26, 40));

        assert!(notepad_caret_position(&app, panel).is_none());

        app.notepad.focused = true;
        assert!(notepad_caret_position(&app, panel).is_some());

        // A caret scrolled out of the panel's rows is not drawn at its edge.
        app.notepad.cursor_line = 99;
        assert!(notepad_caret_position(&app, panel).is_none());

        // The error row replaces the body, so there is nothing to point at.
        app.notepad.cursor_line = 0;
        app.notepad.error = Some("cannot save".into());
        assert!(notepad_caret_position(&app, panel).is_none());

        // No panel, no caret.
        app.notepad.error = None;
        assert!(notepad_caret_position(&app, Rect::default()).is_none());
    }
}
