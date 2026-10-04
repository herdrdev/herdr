//! Scratch Notes projection; cached data only, no filesystem work in rendering.
use super::{workspace_list_body_rect, workspace_list_rect_for_app, SidebarRow};
use crate::app::AppState;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
    Frame,
};
#[derive(Clone, Debug)]
pub(crate) enum NotesLine {
    Header,
    Note(usize),
    More,
}
pub(super) fn append_rows(app: &AppState, rows: &mut Vec<SidebarRow>) {
    rows.push(SidebarRow::Notes(NotesLine::Header));
    for i in 0..app.scratch.visible_count() {
        rows.push(SidebarRow::Notes(NotesLine::Note(i)));
    }
    if !app.scratch.collapsed && app.scratch.notes.len() > 3 {
        rows.push(SidebarRow::Notes(NotesLine::More));
    }
}
pub(crate) fn areas(app: &AppState, area: Rect) -> Vec<(NotesLine, Rect)> {
    let ws = workspace_list_rect_for_app(app, area);
    let metrics = super::workspace_list_scroll_metrics(app, ws);
    let body = workspace_list_body_rect(
        app,
        ws,
        crate::ui::scrollbar::should_show_scrollbar(metrics),
    );
    let rows = super::sidebar_rows(app);
    let mut y = body.y;
    let mut out = Vec::new();
    for (idx, row) in rows
        .iter()
        .enumerate()
        .skip(app.workspace_scroll.min(metrics.max_offset_from_bottom))
    {
        let height = super::sidebar_row_height(app, row, body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        if let SidebarRow::Notes(line) = row {
            out.push((line.clone(), Rect::new(body.x, y, body.width, height)));
        }
        y = y
            .saturating_add(height)
            .saturating_add(super::sidebar_row_gap(app, &rows, idx));
    }
    out
}
pub(crate) fn hover(app: &AppState, line: &NotesLine) -> String {
    match line {
        NotesLine::Header => "Notes · +: new note · click header to collapse".into(),
        NotesLine::Note(i) => app
            .scratch
            .notes
            .get(*i)
            .map(|n| {
                format!(
                    "{} · {}{}",
                    n.title,
                    crate::ui::scratch::date(n.modified, false),
                    if n.synced {
                        " · synced with Zen writer"
                    } else {
                        " · local"
                    }
                )
            })
            .unwrap_or_default(),
        NotesLine::More => "Expand or collapse the remaining notes".into(),
    }
}
pub(crate) fn label(app: &AppState, line: &NotesLine, width: usize) -> String {
    match line {
        NotesLine::Header => format!(
            "{} {}Notes",
            if app.scratch.collapsed { "▸" } else { "▾" },
            if app.nerd_font { "✎ " } else { "" }
        ),
        NotesLine::Note(i) => {
            let Some(note) = app.scratch.notes.get(*i) else {
                return String::new();
            };
            let tail = format!(
                " {} {}",
                crate::ui::scratch::date(note.modified, true),
                if note.synced {
                    if app.nerd_font {
                        "⇅"
                    } else {
                        "sync"
                    }
                } else {
                    ""
                }
            );
            let title = crate::ui::text::truncate_end(
                &note.title,
                width.saturating_sub(unicode_width::UnicodeWidthStr::width(tail.as_str()) + 2),
            );
            format!("  {title}{tail}")
        }
        NotesLine::More => format!(
            "  {} {} more",
            if app.scratch.expanded { "▾" } else { "▸" },
            app.scratch.notes.len().saturating_sub(3)
        ),
    }
}
pub(crate) fn render(app: &AppState, frame: &mut Frame, line: &NotesLine, rect: Rect) {
    let text = label(app, line, usize::from(rect.width));
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(app.palette.text).add_modifier(
            if matches!(line, NotesLine::Header) {
                Modifier::BOLD
            } else {
                Modifier::empty()
            },
        )),
        rect,
    );
    if matches!(line, NotesLine::Header) && rect.width > 2 {
        let glyph = if app.nerd_font { "＋" } else { "+" };
        frame.render_widget(
            Paragraph::new(glyph).style(Style::default().fg(app.palette.text)),
            Rect::new(rect.right() - 2, rect.y, 2, 1),
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scratch_notes_precede_inbox_and_header_has_fallback() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.fleet_snapshot.polled = true;
        app.fleet_snapshot.inbox = Some(std::sync::Arc::new(crate::inbox::ProducerSnapshot::read(
            "test".into(),
            crate::inbox::Healthcheck::default(),
            std::time::SystemTime::now(),
        )));
        let mut rows = Vec::new();
        append_rows(&app, &mut rows);
        super::super::inbox::append_rows(&app, &mut rows);
        assert!(matches!(rows.last(), Some(SidebarRow::Inbox(_))));
        assert!(matches!(rows[0], SidebarRow::Notes(NotesLine::Header)));
        let backend = ratatui::backend::TestBackend::new(18, 2);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(&app, frame, &NotesLine::Header, Rect::new(0, 0, 18, 1)))
            .expect("draw");
        let output = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(output.contains("Notes") && output.contains('+'));
        assert!(hover(&app, &NotesLine::Header).contains("Notes"));
    }
    #[test]
    fn scratch_notes_keep_title_time_and_sync_at_narrow_and_normal_widths() {
        let mut app = AppState::test_new();
        app.scratch.new_note();
        let mut note = app.scratch.editor.as_ref().expect("editor").note.clone();
        note.title = "Freeze notes for Herdr with a long title".into();
        note.synced = true;
        app.scratch.notes = vec![note];
        for font in [true, false] {
            app.nerd_font = font;
            for width in [18u16, 48] {
                let text = label(&app, &NotesLine::Note(0), usize::from(width));
                assert!(text.contains("Fr"));
                assert!(text.contains(&crate::ui::scratch::date(
                    app.scratch.notes[0].modified,
                    true
                )));
                assert!(text.contains(if font { "⇅" } else { "sync" }));
                assert!(unicode_width::UnicodeWidthStr::width(text.as_str()) <= usize::from(width));
                let backend = ratatui::backend::TestBackend::new(width, 1);
                let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
                terminal
                    .draw(|frame| {
                        render(&app, frame, &NotesLine::Note(0), Rect::new(0, 0, width, 1))
                    })
                    .expect("render");
                let output = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(output.contains(if font { "⇅" } else { "sync" }));
            }
        }
    }
}
