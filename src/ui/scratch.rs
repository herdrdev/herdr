//! Pure full-pane writer/list renderer, matching the approved Scratch sketch.
use crate::app::AppState;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub(crate) fn writer_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(64);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + area.height.min(4),
        width,
        area.height.saturating_sub(8),
    )
}
pub(crate) fn date(at: std::time::SystemTime, short: bool) -> String {
    let fallback = time::OffsetDateTime::from(at);
    let stamp = at
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| crate::platform::local_datetime_at(d.as_secs()))
        .unwrap_or_else(|| time::PrimitiveDateTime::new(fallback.date(), fallback.time()));
    if short && crate::platform::local_datetime().is_some_and(|now| now.date() != stamp.date()) {
        return stamp.weekday().to_string().chars().take(3).collect();
    }
    if short {
        format!("{:02}:{:02}", stamp.hour(), stamp.minute())
    } else {
        format!(
            "{} · {:02}:{:02}",
            stamp.date(),
            stamp.hour(),
            stamp.minute()
        )
    }
}
pub(crate) fn render(app: &AppState, frame: &mut Frame, area: Rect) {
    let state = &app.scratch;
    let column = writer_rect(area);
    let dim = Style::default().fg(app.palette.overlay0);
    let text = Style::default().fg(app.palette.text);
    if state.list {
        let count = usize::from(column.height.saturating_sub(3));
        let start = state.selected.saturating_sub(count.saturating_sub(1));
        let mut rows = vec![Line::from(Span::styled(
            "Notes",
            text.add_modifier(Modifier::BOLD),
        ))];
        for (i, note) in state.notes.iter().enumerate().skip(start).take(count) {
            let tail = format!(
                "   {} {}",
                date(note.modified, false),
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
                usize::from(column.width)
                    .saturating_sub(unicode_width::UnicodeWidthStr::width(tail.as_str()) + 2),
            );
            rows.push(Line::from(vec![
                Span::styled(
                    format!("{} {title}", if i == state.selected { ">" } else { " " }),
                    if i == state.selected {
                        text.add_modifier(Modifier::BOLD)
                    } else {
                        text
                    },
                ),
                Span::styled(tail, dim),
            ]));
        }
        if state.notes.is_empty() {
            rows.push(Line::from(Span::styled("No notes · n: new", dim)));
        }
        frame.render_widget(Paragraph::new(rows), column);
    } else if let Some(editor) = &state.editor {
        frame.render_widget(
            Paragraph::new(date(editor.note.modified, false)).style(dim),
            Rect::new(
                column.x,
                column.y.saturating_sub(2).max(area.y),
                column.width,
                1,
            ),
        );
        let (rows, cursor) = editor.rows(usize::from(column.width));
        let start = editor
            .scroll
            .max(
                cursor
                    .0
                    .saturating_sub(usize::from(column.height.saturating_sub(1))),
            )
            .min(cursor.0);
        let lines = rows
            .into_iter()
            .skip(start)
            .take(usize::from(column.height))
            .map(Line::from)
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines).style(text), column);
        if column.width > 0 && column.height > 0 {
            frame.set_cursor_position((
                column.x + (cursor.1 as u16).min(column.width - 1),
                column.y + ((cursor.0 - start) as u16).min(column.height - 1),
            ));
        }
    }
    let footer = if state.confirm_delete {
        "Delete note? y/enter: delete · n/esc: cancel".into()
    } else if let Some(error) = &state.error {
        format!("save error: {error}")
    } else if state.list {
        "enter: open · n: new · s: sync · d: delete · esc: back".into()
    } else {
        state
            .editor
            .as_ref()
            .map(|e| {
                format!(
                    "{} words · {} · esc: notes",
                    e.words(),
                    if e.dirty_at.is_some() {
                        "saving"
                    } else {
                        "saved"
                    }
                )
            })
            .unwrap_or_default()
    };
    frame.render_widget(
        Paragraph::new(footer).style(if state.error.is_some() {
            Style::default().fg(app.palette.red)
        } else {
            dim
        }),
        Rect::new(
            column.x,
            area.bottom().saturating_sub(2).max(area.y),
            column.width,
            1,
        ),
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scratch_writer_is_centered_and_renders_footer_and_date() {
        let mut app = AppState::test_new();
        app.scratch.new_note();
        let e = app.scratch.editor.as_mut().expect("editor");
        e.insert("hello world");
        e.dirty_at = None;
        let area = Rect::new(0, 0, 100, 30);
        let column = writer_rect(area);
        assert_eq!(column.x, 18);
        assert_eq!(column.width, 64);
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(&app, frame, area))
            .expect("draw");
        let output = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(output.contains("2 words · saved · esc: notes"));
        assert!(output.contains("hello world"));
    }
    #[test]
    fn scratch_list_renders_title_modified_time_and_sync() {
        let mut app = AppState::test_new();
        app.scratch.new_note();
        let mut note = app.scratch.editor.as_ref().expect("editor").note.clone();
        note.title = "Tax checklist questions".into();
        note.synced = true;
        let modified = date(note.modified, false);
        app.scratch.notes = vec![note];
        app.scratch.list = true;
        for font in [true, false] {
            app.nerd_font = font;
            let backend = ratatui::backend::TestBackend::new(100, 30);
            let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| render(&app, frame, Rect::new(0, 0, 100, 30)))
                .expect("draw");
            let output = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(output.contains("Tax checklist questions"));
            assert!(output.contains(&modified));
            assert!(output.contains(if font { "⇅" } else { "sync" }));
        }
    }
}
