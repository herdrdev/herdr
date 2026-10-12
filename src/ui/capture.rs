use crate::app::state::AppState;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub(crate) fn capture_rect(app: &AppState, screen: Rect) -> Option<Rect> {
    let state = app.capture.as_ref()?;
    let width = screen.width.saturating_sub(2);
    let text_width = width.saturating_sub(4).max(1);
    let lines = crate::ui::home::prompt_layout(&state.text, text_width, u16::MAX, true)
        .0
        .len()
        .max(1);
    let rows = u16::try_from(lines)
        .unwrap_or(u16::MAX)
        .clamp(2, (screen.height / 4).max(2));
    let height = (6 + rows).min(screen.height.saturating_sub(1).max(1));
    Some(Rect::new(
        screen.x.saturating_add(1),
        screen
            .y
            .saturating_add(screen.height.saturating_sub(height).saturating_sub(1)),
        width,
        height,
    ))
}

pub(crate) fn hit_test(app: &AppState, screen: Rect, x: u16, y: u16) -> bool {
    capture_rect(app, screen)
        .is_some_and(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
}

pub(crate) fn render(app: &AppState, frame: &mut Frame, screen: Rect) {
    let Some(state) = app.capture.as_ref() else {
        return;
    };
    let Some(rect) = capture_rect(app, screen) else {
        return;
    };
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let buffer = frame.buffer_mut();
    for y in buffer.area.y..buffer.area.bottom() {
        for x in buffer.area.x..buffer.area.right() {
            if x < rect.x || x >= rect.right() || y < rect.y || y >= rect.bottom() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_fg(app.palette.overlay1);
                    cell.set_style(Style::default().remove_modifier(Modifier::BOLD));
                }
            }
        }
    }
    let panel = Style::default().bg(app.palette.panel_bg);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.palette.accent))
            .style(panel),
        rect,
    );
    let heading = state.error.as_ref().map_or_else(
        || " capture ".to_string(),
        |e| format!(" could not save: {e} "),
    );
    let color = if state.error.is_some() {
        app.palette.red
    } else {
        app.palette.accent
    };
    line(
        frame,
        Rect::new(rect.x + 1, rect.y, rect.width.saturating_sub(2), 1),
        vec![Span::styled(
            heading,
            Style::default()
                .fg(color)
                .bg(app.palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        )],
    );
    if let Some(age) = &state.restored_age {
        let text = format!("draft restored · {age}");
        let width = text.chars().count() as u16;
        let x = rect.right().saturating_sub(width + 1).max(rect.x + 1);
        line(
            frame,
            Rect::new(
                x,
                rect.y,
                rect.right().saturating_sub(1).saturating_sub(x),
                1,
            ),
            vec![Span::styled(
                text,
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg),
            )],
        );
    }
    if rect.height < 6 {
        return;
    }
    let text_area = Rect::new(
        rect.x.saturating_add(2),
        rect.y.saturating_add(2),
        rect.width.saturating_sub(4),
        rect.height.saturating_sub(6),
    );
    let (mut rows, cursor) =
        crate::ui::home::prompt_layout(&state.text, text_area.width, text_area.height, true);
    let empty = state.text.is_empty();
    if empty {
        rows = vec!["dictate the whole thought".into()];
    }
    let text_style = if empty {
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default()
            .fg(app.palette.text)
            .add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        Paragraph::new(
            rows.into_iter()
                .map(|s| Line::from(Span::styled(s, text_style)))
                .collect::<Vec<_>>(),
        )
        .style(panel),
        text_area,
    );
    let divider_y = rect.y.saturating_add(rect.height.saturating_sub(4));
    let divider_rect = Rect::new(
        rect.x.saturating_add(1),
        divider_y,
        rect.width.saturating_sub(2),
        1,
    );
    line(
        frame,
        divider_rect,
        vec![Span::styled(
            "─".repeat(divider_rect.width as usize),
            Style::default()
                .fg(app.palette.surface_dim)
                .bg(app.palette.panel_bg),
        )],
    );
    footer(app, frame, rect);
    if text_area.width > 0 && text_area.height > 0 {
        let (x, y) = cursor.unwrap_or((0, 0));
        frame.set_cursor_position((
            text_area.x + x.min(text_area.width - 1),
            text_area.y + y.min(text_area.height - 1),
        ));
    }
}

fn footer(app: &AppState, frame: &mut Frame, rect: Rect) {
    if rect.height < 5 {
        return;
    }
    let left = [("↵  ", "save"), ("⌃↵ ", "save + research")];
    let center = ["→ focus list · dumped", "→ focus list · ◆ research queued"];
    let right = [("⇧↵  ", "new line  "), ("esc ", "keep draft")];
    for i in 0..2usize {
        let width = rect.width.saturating_sub(4);
        let left_width = 22_u16.min(width);
        let show_right = width >= 58;
        let right_text = if show_right {
            format!("{}{}", right[i].0, right[i].1)
        } else {
            String::new()
        };
        let right_width = right_text.chars().count() as u16;
        let center_width =
            width.saturating_sub(left_width + right_width + if show_right { 4 } else { 2 });
        let center_text = if center_width >= center[i].chars().count() as u16 {
            center[i]
        } else if i == 0 {
            "→ dumped"
        } else {
            "→ ◆ research queued"
        };
        let center_text: String = center_text.chars().take(center_width as usize).collect();
        let gap =
            width.saturating_sub(left_width + center_text.chars().count() as u16 + right_width);
        let mut spans =
            vec![
                Span::styled(
                    left[i].0,
                    Style::default()
                        .fg(app.palette.accent)
                        .bg(app.palette.panel_bg),
                ),
                Span::styled(
                    left[i].1,
                    Style::default()
                        .fg(app.palette.subtext0)
                        .bg(app.palette.panel_bg),
                ),
                Span::raw(" ".repeat(
                    left_width.saturating_sub(3 + left[i].1.chars().count() as u16) as usize,
                )),
                Span::raw(" ".repeat(gap as usize / 2)),
                Span::styled(
                    center_text,
                    Style::default()
                        .fg(if i == 0 {
                            app.palette.subtext0
                        } else {
                            app.palette.teal
                        })
                        .bg(app.palette.panel_bg),
                ),
                Span::raw(" ".repeat(gap as usize - gap as usize / 2)),
            ];
        if show_right {
            spans.push(Span::styled(
                right[i].0,
                Style::default()
                    .fg(app.palette.accent)
                    .bg(app.palette.panel_bg),
            ));
            spans.push(Span::styled(
                right[i].1,
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg),
            ));
        }
        line(
            frame,
            Rect::new(rect.x + 2, rect.y + rect.height - 3 + i as u16, width, 1),
            spans,
        );
    }
}

fn line(frame: &mut Frame, rect: Rect, spans: Vec<Span<'static>>) {
    if rect.width > 0 && rect.height > 0 {
        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    }
}

pub(crate) fn render_ack(app: &AppState, frame: &mut Frame) {
    let Some(ack) = app.capture_ack.as_ref() else {
        return;
    };
    let text = ack.text.as_str();
    let available = if app.status_bar_enabled && app.view.status_bar_rect.height > 0 {
        app.view.status_bar_rect
    } else {
        Rect::new(
            0,
            frame.area().bottom().saturating_sub(1),
            frame.area().width,
            1,
        )
    };
    let label = format!("✓ {text}");
    let width = (label.chars().count() as u16).min(available.width);
    if width == 0 {
        return;
    }
    let area = if app.status_bar_enabled && app.view.status_bar_rect.height > 0 {
        crate::ui::status::capture_ack_rect(app, available, width).unwrap_or(available)
    } else {
        available
    };
    if app.status_bar_enabled && app.view.status_bar_rect.height > 0 {
        frame.render_widget(
            Paragraph::new(" ".repeat(usize::from(area.width)))
                .style(Style::default().bg(app.palette.panel_bg)),
            area,
        );
    }
    let x = if app.status_bar_enabled && app.view.status_bar_rect.height > 0 {
        area.x + area.width.saturating_sub(width) / 2
    } else {
        area.x + (area.width - width) / 2
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("✓", Style::default().fg(app.palette.green)),
            Span::styled(format!(" {text}"), Style::default().fg(app.palette.text)),
        ])),
        Rect::new(x, area.y, width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn draw(width: u16, height: u16, text: &str) -> (AppState, Terminal<TestBackend>) {
        let mut app = AppState::test_new();
        app.capture = Some(crate::app::capture::CaptureState {
            text: text.into(),
            restored_age: None,
            error: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(&app, frame, frame.area()))
            .unwrap();
        (app, terminal)
    }
    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        (0..terminal.backend().buffer().area.width)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn geometry_grows_caps_scrolls_and_leaves_floor_and_sides() {
        let (app, _) = draw(160, 32, "a");
        let rect = capture_rect(&app, Rect::new(0, 0, 160, 32)).unwrap();
        assert_eq!(rect, Rect::new(1, 23, 158, 8));
        let (app, _) = draw(40, 32, &"a".repeat(100));
        let rect = capture_rect(&app, Rect::new(0, 0, 40, 32)).unwrap();
        assert_eq!(rect.height, 9);
        let long = "a\n".repeat(40);
        let (app, _) = draw(40, 20, &long);
        let rect = capture_rect(&app, Rect::new(0, 0, 40, 20)).unwrap();
        assert_eq!(rect.height, 11);
        assert_eq!(rect.bottom(), 19);
        assert!(hit_test(&app, Rect::new(0, 0, 40, 20), rect.x, rect.y));
        let (tiny, _) = draw(20, 10, "tiny");
        let tiny_rect = capture_rect(&tiny, Rect::new(0, 0, 20, 10)).unwrap();
        assert!(tiny_rect.right() <= 20 && tiny_rect.bottom() <= 10);
    }

    #[test]
    fn render_footer_and_dimmed_underlay() {
        let (app, terminal) = draw(160, 32, "");
        let rect = capture_rect(&app, terminal.backend().buffer().area).unwrap();
        let footer1 = row(&terminal, rect.y + rect.height - 3);
        let footer2 = row(&terminal, rect.y + rect.height - 2);
        assert!(footer1.contains("↵  save"));
        assert!(footer1.contains("→ focus list · dumped"));
        assert!(footer1.contains("⇧↵  new line"));
        assert!(footer2.contains("⌃↵ save + research"));
        assert!(footer2.contains("◆ research queued"));
        assert!(footer2.contains("esc keep draft"));
        let cell = &terminal.backend().buffer()[(0, 0)];
        assert_eq!(cell.fg, app.palette.overlay1);
        assert!(!cell.modifier.contains(Modifier::BOLD));
        let (_, narrow) = draw(100, 24, "");
        let narrow_area = narrow.backend().buffer().area;
        let narrow_rect = Rect::new(1, narrow_area.height - 9, 98, 8);
        assert!(row(&narrow, narrow_rect.y + 5).contains("save"));
        assert!(row(&narrow, narrow_rect.y + 6).contains("save + research"));
        let mut closed = AppState::test_new();
        let mut empty = Terminal::new(TestBackend::new(30, 10)).unwrap();
        empty
            .draw(|frame| {
                frame.render_widget(Paragraph::new("original"), frame.area());
                render(&closed, frame, frame.area());
            })
            .unwrap();
        assert!(row(&empty, 0).contains("original"));
        closed.capture = None;
    }

    #[test]
    fn render_clears_underlay_inside_composer() {
        let mut app = AppState::test_new();
        app.capture = Some(crate::app::capture::CaptureState {
            text: "hello".into(),
            restored_age: None,
            error: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(Paragraph::new("  󰄻│M↵ save\n".repeat(20)), area);
                render(&app, frame, area);
            })
            .unwrap();
        let rect = capture_rect(&app, terminal.backend().buffer().area).unwrap();
        let blank_y = rect.y + 1;
        for x in rect.x + 1..rect.right() - 1 {
            assert_eq!(terminal.backend().buffer()[(x, blank_y)].symbol(), " ");
        }
        let text_y = rect.y + 2;
        assert_eq!(
            terminal.backend().buffer()[(rect.x + 2, text_y)].symbol(),
            "h"
        );
        let cursor_x = rect.x + 2 + 5;
        assert_eq!(
            terminal.backend().buffer()[(cursor_x, text_y)].symbol(),
            "█"
        );
        for x in cursor_x + 1..rect.right() - 2 {
            assert_eq!(terminal.backend().buffer()[(x, text_y)].symbol(), " ");
        }
        for y in rect.y + 1..rect.bottom() - 1 {
            assert_eq!(terminal.backend().buffer()[(rect.x, y)].symbol(), "│");
            assert_eq!(
                terminal.backend().buffer()[(rect.right() - 1, y)].symbol(),
                "│"
            );
        }
    }
}
