//! Pure geometry and rendering for the focused session's goals panel.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::text::{display_width, truncate_end};
use crate::{
    app::AppState,
    goals::{GoalsFile, GoalsLoad, Stream, StreamState},
};

const MIN_PANEL_ROWS: u16 = 2;
const MAX_PANEL_ROWS: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SidebarPanelRects {
    pub(crate) workspaces: Rect,
    pub(crate) notepad: Rect,
    pub(crate) goals: Rect,
}

pub(crate) fn split_sidebar_panels(app: &AppState, content: Rect) -> SidebarPanelRects {
    let goals_height = goals_height(app, content);
    let goals = if goals_height == 0 {
        Rect::default()
    } else {
        Rect::new(
            content.x,
            content.bottom().saturating_sub(goals_height),
            content.width,
            goals_height,
        )
    };
    let upper = Rect::new(
        content.x,
        content.y,
        content.width,
        content.height.saturating_sub(goals_height),
    );
    let notepad = if app.sidebar_sections_layout && !app.sidebar_areas.notes {
        Rect::default()
    } else {
        super::notepad::notepad_panel_rect(app, upper)
    };
    let workspaces = Rect::new(
        upper.x,
        upper.y,
        upper.width,
        upper.height.saturating_sub(notepad.height),
    );
    SidebarPanelRects {
        workspaces,
        notepad,
        goals,
    }
}

fn goals_height(app: &AppState, content: Rect) -> u16 {
    if !app.goals.enabled || app.sidebar_collapsed || content.width == 0 {
        return 0;
    }
    let wanted = match &app.goals.load {
        GoalsLoad::Missing => return 0,
        GoalsLoad::Malformed(_) => MIN_PANEL_ROWS,
        GoalsLoad::Ready(file) => visible_line_count(file)
            .saturating_add(1)
            .min(usize::from(u16::MAX)) as u16,
    }
    .clamp(MIN_PANEL_ROWS, MAX_PANEL_ROWS);
    let spare = content
        .height
        .saturating_sub(super::sidebar::sidebar_workspace_floor(app));
    if spare >= MIN_PANEL_ROWS {
        wanted.min(spare)
    } else {
        0
    }
}

fn visible_line_count(file: &GoalsFile) -> usize {
    let streams = file
        .streams
        .values()
        .filter(|stream| file.goals.contains_key(&stream.goal))
        .count();
    file.goals.len().saturating_add(streams).max(1)
}

pub(crate) fn render_goals(app: &AppState, frame: &mut Frame, panel: Rect) {
    if panel.width == 0 || panel.height == 0 {
        return;
    }
    render_header(app, frame, panel);
    let body = Rect::new(
        panel.x,
        panel.y.saturating_add(1),
        panel.width,
        panel.height.saturating_sub(1),
    );
    if body.height == 0 {
        return;
    }

    match &app.goals.load {
        GoalsLoad::Missing => {}
        GoalsLoad::Malformed(_) => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_end("invalid .streams.json", usize::from(body.width)),
                    Style::default()
                        .fg(app.palette.overlay0)
                        .add_modifier(Modifier::DIM),
                )),
                Rect::new(body.x, body.y, body.width, 1),
            );
        }
        GoalsLoad::Ready(file) => render_file(app, frame, body, file),
    }
}

fn render_header(app: &AppState, frame: &mut Frame, panel: Rect) {
    let style = Style::default().fg(app.palette.surface_dim);
    let buffer = frame.buffer_mut();
    for x in panel.x..panel.right() {
        buffer[(x, panel.y)].set_symbol("─");
        buffer[(x, panel.y)].set_style(style);
    }
    if panel.width > 0 {
        buffer[(panel.x, panel.y)].set_symbol("├");
    }
    if panel.width > 1 {
        buffer[(panel.right() - 1, panel.y)].set_symbol("┤");
        let title = truncate_end(
            " Goals (focused session) ",
            usize::from(panel.width.saturating_sub(2)),
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                title,
                Style::default().fg(app.palette.subtext0),
            )),
            Rect::new(
                panel.x.saturating_add(1),
                panel.y,
                panel.width.saturating_sub(2),
                1,
            ),
        );
    }
}

fn render_file(app: &AppState, frame: &mut Frame, body: Rect, file: &GoalsFile) {
    let mut row = 0u16;
    let mut goals: Vec<_> = file.goals.iter().collect();
    goals.sort_by_key(|(id, _)| numeric_id(id));

    if goals.is_empty() {
        render_plain_row(
            frame,
            body,
            row,
            "no goals",
            Style::default()
                .fg(app.palette.overlay0)
                .add_modifier(Modifier::DIM),
        );
        return;
    }

    for (goal_id, goal) in goals {
        if row >= body.height {
            break;
        }
        render_plain_row(
            frame,
            body,
            row,
            &format!("{goal_id} {}", goal.text),
            Style::default().fg(app.palette.text),
        );
        row = row.saturating_add(1);

        let mut streams: Vec<_> = file
            .streams
            .iter()
            .filter(|(_, stream)| stream.goal == *goal_id)
            .collect();
        streams.sort_by_key(|(id, _)| numeric_id(id));
        for (stream_id, stream) in streams {
            if row >= body.height {
                return;
            }
            render_stream_row(app, frame, body, row, stream_id, stream);
            row = row.saturating_add(1);
        }
    }
}

fn render_plain_row(frame: &mut Frame, body: Rect, row: u16, text: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_end(text, usize::from(body.width)),
            style,
        )),
        Rect::new(body.x, body.y.saturating_add(row), body.width, 1),
    );
}

fn render_stream_row(
    app: &AppState,
    frame: &mut Frame,
    body: Rect,
    row: u16,
    stream_id: &str,
    stream: &Stream,
) {
    let (status, color) = if stream.needs.is_some() {
        ("⏸ needs you", app.palette.red)
    } else {
        match stream.state {
            StreamState::Running => ("◐ running", app.palette.yellow),
            StreamState::Waiting => ("⏸ waiting", app.palette.overlay1),
            StreamState::Blocked => ("● blocked", app.palette.red),
            StreamState::Done => ("✓ done", app.palette.green),
        }
    };
    let status_width = display_width(status);
    let width = usize::from(body.width);
    let left_budget = width.saturating_sub(status_width.saturating_add(1));
    let left = truncate_end(&format!("  {stream_id} {}", stream.what), left_budget);
    let gap = " ".repeat(width.saturating_sub(display_width(&left).saturating_add(status_width)));
    let mut left_style = Style::default().fg(app.palette.subtext0);
    let mut status_style = Style::default().fg(color);
    if stream.state == StreamState::Done {
        left_style = left_style.add_modifier(Modifier::DIM);
        status_style = status_style.add_modifier(Modifier::DIM);
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left, left_style),
            Span::raw(gap),
            Span::styled(status, status_style),
        ])),
        Rect::new(body.x, body.y.saturating_add(row), body.width, 1),
    );
}

fn numeric_id(id: &str) -> u64 {
    id.get(1..)
        .and_then(|number| number.parse().ok())
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;

    const FILE: &str = r#"{
        "version": 1,
        "next_goal_id": 11,
        "next_stream_id": 11,
        "goals": {
            "G10": {"text": "later goal", "done_when": "later"},
            "G2": {"text": "visible goal", "done_when": "visible"}
        },
        "streams": {
            "S10": {"goal": "G10", "what": "finished work", "state": "done", "owner": "agent"},
            "S2": {"goal": "G2", "what": "preview", "state": "blocked", "owner": "agent", "needs": []}
        }
    }"#;

    fn app(load: GoalsLoad) -> AppState {
        let mut app = AppState::test_new();
        app.goals.enabled = true;
        app.goals.load = load;
        app
    }

    fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(terminal.backend().buffer().area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    #[test]
    fn renders_numeric_order_needs_and_dimmed_done_streams() {
        let app = app(GoalsLoad::Ready(
            crate::goals::parse(FILE).expect("fixture"),
        ));
        let area = Rect::new(0, 0, 36, 6);
        let mut terminal = Terminal::new(TestBackend::new(36, 6)).expect("terminal");
        terminal
            .draw(|frame| render_goals(&app, frame, area))
            .expect("render");
        let rows = rows(&terminal);

        assert!(rows[1].contains("G2 visible goal"));
        assert!(rows[2].contains("S2 preview"));
        assert!(rows[2].contains("needs you"));
        assert!(rows[3].contains("G10 later goal"));
        assert!(rows[4].contains("S10 finished work"));
        assert!(terminal.backend().buffer()[(2, 4)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
    }

    #[test]
    fn malformed_file_renders_one_dim_error_line() {
        let app = app(GoalsLoad::Malformed("bad json".into()));
        let area = Rect::new(0, 0, 30, 2);
        let mut terminal = Terminal::new(TestBackend::new(30, 2)).expect("terminal");
        terminal
            .draw(|frame| render_goals(&app, frame, area))
            .expect("render");

        assert!(rows(&terminal)[1].contains("invalid .streams.json"));
        assert!(terminal.backend().buffer()[(0, 1)]
            .style()
            .add_modifier
            .contains(Modifier::DIM));
    }

    #[test]
    fn panel_split_preserves_workspace_floor_and_does_not_overlap_notepad() {
        let mut app = app(GoalsLoad::Ready(
            crate::goals::parse(FILE).expect("fixture"),
        ));
        app.notepad.enabled = true;
        app.notepad.height = 5;
        app.notepad.set_visible_tabs(vec!["usage".to_string()]);
        let rects = split_sidebar_panels(&app, Rect::new(0, 1, 36, 12));

        assert!(rects.workspaces.height >= crate::ui::sidebar::sidebar_workspace_floor(&app));
        assert!(rects.workspaces.bottom() <= rects.notepad.y);
        assert!(rects.notepad.bottom() <= rects.goals.y);
        assert_eq!(rects.goals.bottom(), 13);
    }

    #[test]
    fn hidden_goals_reserve_no_sidebar_rows() {
        let mut app = app(GoalsLoad::Ready(
            crate::goals::parse(FILE).expect("fixture"),
        ));
        app.goals.enabled = false;
        let content = Rect::new(0, 1, 36, 20);
        let rects = split_sidebar_panels(&app, content);

        assert_eq!(rects.goals, Rect::default());
        assert_eq!(rects.workspaces, content);
    }
}
