//! Pure renderer for the Focus view: one empty, full-screen notepad.
use crate::app::AppState;
use ratatui::{layout::Rect, style::Style, text::Line, widgets::Paragraph, Frame};

/// What an empty Focus notepad says, so the view never reads as broken.
pub(crate) const PLACEHOLDER: &str = "Write anything. Esc returns to Home.";

pub(crate) fn render(app: &AppState, frame: &mut Frame, area: Rect) {
    let column = super::scratch::writer_rect(area);
    if column.width == 0 || column.height == 0 {
        return;
    }
    let Some(editor) = app.focus_editor.as_ref() else {
        return;
    };
    if editor.note.body.is_empty() {
        frame.render_widget(
            Paragraph::new(PLACEHOLDER).style(Style::default().fg(app.palette.overlay0)),
            column,
        );
        frame.set_cursor_position((column.x, column.y));
        return;
    }
    let (rows, cursor) = editor.rows(usize::from(column.width));
    let start = cursor
        .0
        .saturating_sub(usize::from(column.height.saturating_sub(1)));
    let lines = rows
        .into_iter()
        .skip(start)
        .take(usize::from(column.height))
        .map(Line::from)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(app.palette.text)),
        column,
    );
    frame.set_cursor_position((
        column.x
            + u16::try_from(cursor.1)
                .unwrap_or(u16::MAX)
                .min(column.width - 1),
        column.y
            + u16::try_from(cursor.0 - start)
                .unwrap_or(u16::MAX)
                .min(column.height - 1),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn rows(app: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(app, frame, Rect::new(0, 0, width, height)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn an_empty_focus_notepad_shows_its_placeholder() {
        let mut app = AppState::test_new();
        app.open_focus_view();
        let rendered = rows(&app, 80, 20);
        assert!(
            rendered.iter().any(|row| row.contains(PLACEHOLDER)),
            "{rendered:#?}"
        );
    }

    #[test]
    fn focus_text_replaces_the_placeholder_and_wraps_in_the_writer_column() {
        let mut app = AppState::test_new();
        app.open_focus_view();
        app.focus_editor
            .as_mut()
            .expect("editor")
            .insert("first line\nsecond line");
        let rendered = rows(&app, 80, 20);
        let text = rendered.join("\n");
        assert!(!text.contains(PLACEHOLDER), "{text}");
        let first = rendered
            .iter()
            .position(|row| row.contains("first line"))
            .expect("first line");
        assert!(rendered[first + 1].contains("second line"), "{text}");
        // Same centred column as the scratch writer.
        let column = super::super::scratch::writer_rect(Rect::new(0, 0, 80, 20));
        assert_eq!(
            rendered[first].find("first line"),
            Some(usize::from(column.x))
        );
    }

    #[test]
    fn the_terminal_area_shows_focus_while_it_is_open() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.active = Some(0);
        app.open_focus_view();
        assert!(matches!(
            app.terminal_area_surface(),
            crate::app::state::TerminalAreaSurface::Focus
        ));
        app.close_top_bar_views();
        assert!(matches!(
            app.terminal_area_surface(),
            crate::app::state::TerminalAreaSurface::Tab
        ));
    }
}
