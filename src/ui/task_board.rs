use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::text::wrap_words;
use super::widgets::{panel_contrast_fg, render_panel_shell};
use crate::app::AppState;
use crate::task::{TaskActivityKind, TaskStatus};

pub(crate) const TASK_BOARD_STATUSES: [TaskStatus; 8] = [
    TaskStatus::Backlog,
    TaskStatus::Ready,
    TaskStatus::Running,
    TaskStatus::Blocked,
    TaskStatus::Review,
    TaskStatus::Done,
    TaskStatus::Failed,
    TaskStatus::Cancelled,
];

pub(super) fn render_task_board_overlay(app: &AppState, frame: &mut Frame) {
    let area = frame.area();
    super::dim_background(frame, area);
    let popup = super::centered_popup_rect(area, 126, 24);
    let Some(popup) = popup else { return };
    let Some(inner) = render_panel_shell(frame, popup, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let header = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 2);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "TASK BOARD",
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  durable work and live agent dispatch",
                Style::default().fg(app.palette.overlay0),
            ),
        ])),
        header,
    );
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        inner.height.saturating_sub(4),
    );
    let columns = Layout::horizontal([Constraint::Ratio(1, 8); 8]).split(body);
    for (column_index, status) in TASK_BOARD_STATUSES.iter().copied().enumerate() {
        render_column(app, frame, columns[column_index], status);
    }
    let footer = Rect::new(
        inner.x + 1,
        inner.y + inner.height.saturating_sub(1),
        inner.width.saturating_sub(2),
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("h/j/k/l", Style::default().fg(app.palette.accent)),
            Span::styled(" navigate  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("enter", Style::default().fg(app.palette.accent)),
            Span::styled(" activity  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("p", Style::default().fg(app.palette.accent)),
            Span::styled(" dock  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("esc/q", Style::default().fg(app.palette.accent)),
            Span::styled(" close", Style::default().fg(app.palette.overlay0)),
        ])),
        footer,
    );
}

pub(super) fn render_task_activity_overlay(app: &AppState, frame: &mut Frame) {
    let area = frame.area();
    super::dim_background(frame, area);
    let Some(popup) = super::centered_popup_rect(area, 94, 28) else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, popup, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let Some(task) = app.tasks.get(app.task_board_selected) else {
        return;
    };

    let header = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 2);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    "TASK ACTIVITY",
                    Style::default()
                        .fg(app.palette.text)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}  {}", task.id, status_label(task.status)),
                    Style::default().fg(app.palette.accent),
                ),
            ]),
            Line::from(Span::styled(
                task.title.as_str(),
                Style::default().fg(app.palette.text),
            )),
        ]),
        header,
    );

    let metadata = Rect::new(
        inner.x + 1,
        inner.y.saturating_add(2),
        inner.width.saturating_sub(2),
        3,
    );
    let agent = task.agent.as_deref().unwrap_or("unassigned");
    let pane = task.pane_id.as_deref().unwrap_or("-");
    let cwd = task.cwd.as_deref().unwrap_or("-");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("agent  ", Style::default().fg(app.palette.overlay0)),
                Span::styled(agent, Style::default().fg(app.palette.text)),
                Span::styled("    pane  ", Style::default().fg(app.palette.overlay0)),
                Span::styled(pane, Style::default().fg(app.palette.text)),
                Span::styled("    attempts  ", Style::default().fg(app.palette.overlay0)),
                Span::styled(
                    task.attempts.to_string(),
                    Style::default().fg(app.palette.text),
                ),
            ]),
            Line::from(vec![
                Span::styled("cwd    ", Style::default().fg(app.palette.overlay0)),
                Span::styled(cwd, Style::default().fg(app.palette.text)),
            ]),
        ]),
        metadata,
    );

    let feed = Rect::new(
        inner.x + 1,
        inner.y.saturating_add(5),
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(7),
    );
    let event_count = task.activities.len();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "ACTIVITY",
                Style::default()
                    .fg(app.palette.overlay0)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "  {event_count} event{}  newest first",
                    if event_count == 1 { "" } else { "s" }
                ),
                Style::default().fg(app.palette.overlay0),
            ),
        ]),
        Line::default(),
    ];
    if task.activities.is_empty() {
        lines.push(Line::from(Span::styled(
            "No recorded activity. This task may predate activity history.",
            Style::default().fg(app.palette.overlay0),
        )));
    } else {
        const MESSAGE_PREFIX: &str = "          | ";
        let message_width = feed
            .width
            .saturating_sub(MESSAGE_PREFIX.len() as u16)
            .max(1) as usize;
        for activity in task.activities.iter().rev() {
            let status = activity
                .status
                .map(|status| format!("[{}]", status_label(status)))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(
                    format_activity_timestamp(activity.timestamp),
                    Style::default().fg(app.palette.overlay0),
                ),
                Span::styled("  + ", Style::default().fg(app.palette.accent)),
                Span::styled(
                    format!("{:<14}", activity_kind_label(activity.kind)),
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(status, Style::default().fg(app.palette.overlay1)),
            ]));
            if let Some(message) = &activity.message {
                for wrapped in wrap_words(message, message_width) {
                    lines.push(Line::from(vec![
                        Span::styled(MESSAGE_PREFIX, Style::default().fg(app.palette.surface1)),
                        Span::styled(wrapped, Style::default().fg(app.palette.text)),
                    ]));
                }
            }
            lines.push(Line::default());
        }
    }
    let max_scroll = lines.len().saturating_sub(feed.height as usize) as u16;
    let paragraph = Paragraph::new(lines);
    frame.render_widget(
        paragraph.scroll((app.task_activity_scroll.min(max_scroll), 0)),
        feed,
    );

    let footer = Rect::new(
        inner.x + 1,
        inner.y + inner.height.saturating_sub(1),
        inner.width.saturating_sub(2),
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("j/k", Style::default().fg(app.palette.accent)),
            Span::styled(" scroll  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("esc", Style::default().fg(app.palette.accent)),
            Span::styled(" board  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("q", Style::default().fg(app.palette.accent)),
            Span::styled(" close", Style::default().fg(app.palette.overlay0)),
        ])),
        footer,
    );
}

pub(crate) fn task_board_task_at(
    app: &AppState,
    screen: Rect,
    column: u16,
    row: u16,
) -> Option<usize> {
    let popup = super::centered_popup_rect(screen, 126, 24)?;
    let inner = Rect::new(
        popup.x.saturating_add(1),
        popup.y.saturating_add(1),
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        inner.height.saturating_sub(4),
    );
    let columns = Layout::horizontal([Constraint::Ratio(1, 8); 8]).split(body);
    for (column_index, status) in TASK_BOARD_STATUSES.iter().copied().enumerate() {
        let area = columns[column_index];
        if column < area.x
            || column >= area.x.saturating_add(area.width.saturating_sub(1))
            || row <= area.y
            || row >= area.y.saturating_add(area.height)
        {
            continue;
        }
        let status_row = usize::from(row - area.y - 1);
        return app
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| task.status == status)
            .nth(status_row)
            .map(|(index, _)| index);
    }
    None
}

pub(super) fn render_task_panel(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width < 8 || area.height < 3 {
        return;
    }

    let placement = if area.x > app.view.terminal_area.x {
        "right"
    } else {
        "bottom"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.palette.surface1))
        .title(format!("tasks | {placement}"));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    frame.render_widget(
        Paragraph::new(format!(
            "{} task{} | click for activity",
            app.tasks.len(),
            if app.tasks.len() == 1 { "" } else { "s" }
        ))
        .style(Style::default().fg(app.palette.overlay0)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let rows = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    );
    for (row, task) in app.tasks.iter().enumerate().take(rows.height as usize) {
        let selected = row == app.task_board_selected;
        let style = if selected {
            Style::default()
                .fg(panel_contrast_fg(&app.palette))
                .bg(app.palette.accent)
        } else {
            Style::default().fg(app.palette.text)
        };
        let label = format!("{}  {}  {}", status_label(task.status), task.id, task.title);
        frame.render_widget(
            Paragraph::new(label).style(style).wrap(Wrap { trim: true }),
            Rect::new(rows.x, rows.y + row as u16, rows.width, 1),
        );
    }
}

fn render_column(app: &AppState, frame: &mut Frame, area: Rect, status: TaskStatus) {
    if area.width < 3 || area.height < 2 {
        return;
    }
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(app.palette.surface1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let title = status_label(status);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let task_rows = app
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.status == status)
        .collect::<Vec<_>>();
    let available = inner.height.saturating_sub(2) as usize;
    for (row, (task_index, task)) in task_rows.into_iter().take(available).enumerate() {
        let selected = task_index == app.task_board_selected;
        let style = if selected {
            Style::default()
                .fg(panel_contrast_fg(&app.palette))
                .bg(app.palette.accent)
        } else {
            Style::default().fg(app.palette.text)
        };
        let label = format!("{} {}", task.id, task.title);
        frame.render_widget(
            Paragraph::new(label).style(style).wrap(Wrap { trim: true }),
            Rect::new(inner.x, inner.y + 1 + row as u16, inner.width, 1),
        );
    }
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Backlog => "backlog",
        TaskStatus::Ready => "ready",
        TaskStatus::Running => "running",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Review => "review",
        TaskStatus::Done => "done",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn activity_kind_label(kind: TaskActivityKind) -> &'static str {
    match kind {
        TaskActivityKind::Created => "created",
        TaskActivityKind::StatusChanged => "status",
        TaskActivityKind::Progress => "progress",
        TaskActivityKind::Attached => "attached",
        TaskActivityKind::Dispatched => "dispatched",
        TaskActivityKind::AgentStatus => "agent status",
        TaskActivityKind::AgentReleased => "agent released",
    }
}

fn format_activity_timestamp(timestamp: u64) -> String {
    let seconds = timestamp % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}
