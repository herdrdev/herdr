use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::state::AppState,
    board::{BoardView, Column, Dialog, EditField, Lane},
};

pub(crate) fn render(app: &AppState, area: Rect, frame: &mut Frame) {
    let Some(view) = app.board_view.as_ref() else {
        return;
    };
    if area.width < 30 || area.height < 12 {
        frame.render_widget(Paragraph::new("Board needs at least 30 × 12 cells"), area);
        return;
    }
    let palette = &app.palette;
    let header = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(
        Paragraph::new(format!("  {}", view.note.header())).style(
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        header,
    );
    if area.width > 12 {
        frame.render_widget(
            Paragraph::new("+ goal").style(Style::default().fg(palette.accent)),
            Rect::new(area.right() - 8, area.y, 7, 1),
        );
    }
    let goal_page_size = goal_page_size(area.width);
    if view.board.goals.len() > goal_page_size && area.width > 26 {
        let first = view.goal_offset + 1;
        let last = (view.goal_offset + goal_page_size).min(view.board.goals.len());
        frame.render_widget(
            Paragraph::new(format!("{first}–{last}/{}", view.board.goals.len()))
                .style(Style::default().fg(palette.subtext0)),
            Rect::new(area.right().saturating_sub(25), area.y, 11, 1),
        );
        frame.render_widget(
            Paragraph::new("← →"),
            Rect::new(area.right() - 13, area.y, 3, 1),
        );
    }
    let goals_height = goals_height(view, area.height);
    let goals_area = Rect::new(area.x, area.y + 1, area.width, goals_height);
    render_goals(app, view, goals_area, frame);
    let board_area = Rect::new(
        area.x,
        goals_area.bottom(),
        area.width,
        area.bottom()
            .saturating_sub(goals_area.bottom())
            .saturating_sub(1),
    );
    render_columns(app, view, board_area, frame);
    let footer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let hint = if view.dialog.is_some() || view.editor.is_some() {
        "Tab field · Enter newline · Ctrl+Enter save · Esc cancel"
    } else if view.detail.is_some() {
        "Tab Human/Agent · e edit · a append · ↑/↓ agent · Enter jump · Esc board"
    } else {
        "←/→ column · ↑/↓ card · Enter detail · m then ←/→ move · n Draft · g goal · [/] goals · s spawn · Esc close"
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(palette.subtext0)),
        footer,
    );
    if let Some(detail) = &view.detail {
        render_detail(app, view, detail, area, frame);
    }
    if let Some(dialog) = &view.dialog {
        render_dialog(app, dialog, area, frame);
    }
    if let Some(editor) = &view.editor {
        render_editor(app, editor, area, frame);
    }
    if let Some(error) = &view.error {
        let rect = Rect::new(
            area.x + 2,
            area.bottom().saturating_sub(3),
            area.width.saturating_sub(4),
            1,
        );
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(palette.red)),
            rect,
        );
    }
}

fn render_goals(app: &AppState, view: &BoardView, area: Rect, frame: &mut Frame) {
    if view.board.goals.is_empty() {
        frame.render_widget(Paragraph::new("  No goals yet · + goal"), area);
        return;
    }
    let cols = view.board.goals.len().clamp(1, goal_page_size(area.width)) as u16;
    let width = area.width / cols;
    for (i, goal) in view
        .board
        .goals
        .iter()
        .skip(view.goal_offset)
        .take(cols as usize)
        .enumerate()
    {
        let x = area.x + width * i as u16;
        let w = if i + 1 == cols as usize {
            area.right() - x
        } else {
            width
        };
        let rect = Rect::new(x, area.y, w, area.height);
        let linked: Vec<_> = view
            .board
            .cards
            .iter()
            .filter(|card| card.goal_id.as_deref() == Some(&goal.id))
            .collect();
        let done = linked
            .iter()
            .filter(|card| card.column == Column::Done)
            .count();
        let icon = if view.board.goal_done(&goal.id) {
            "✓"
        } else {
            "○"
        };
        let mut lines = vec![
            format!("{icon} {}", goal.title),
            format!("{} · {done}/{}", goal.scope.label(), linked.len()),
        ];
        let visible = usize::from(area.height.saturating_sub(4));
        let shown = if linked.len() > visible {
            visible.saturating_sub(1)
        } else {
            visible
        };
        for card in linked.iter().take(shown) {
            lines.push(format!(
                "  {} {}",
                if card.column == Column::Done {
                    "✓"
                } else {
                    "·"
                },
                card.title
            ));
        }
        if linked.len() > shown {
            lines.push(format!("  +{} more", linked.len() - shown));
        }
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .block(Block::default().borders(Borders::ALL))
                .style(Style::default().fg(app.palette.text)),
            rect,
        );
    }
}

pub(crate) fn goal_page_size(width: u16) -> usize {
    usize::from((width / 22).clamp(1, 4))
}

fn goals_height(view: &BoardView, screen_height: u16) -> u16 {
    if view.board.goals.is_empty() {
        return 2;
    }
    8.min((screen_height / 3).max(4))
}

pub(crate) fn column_rects(area: Rect, selected: Column) -> Vec<(Column, Rect)> {
    if area.width == 0 {
        return Vec::new();
    }
    let page_size = if area.width >= 100 { 4 } else { 2 };
    let start = if page_size == 4 || matches!(selected, Column::Draft | Column::Todo) {
        0
    } else {
        2
    };
    (0..page_size)
        .map(|slot| {
            let index = start + slot;
            let width = area.width / page_size as u16;
            let x = area.x + width * slot as u16;
            let w = if slot == page_size - 1 {
                area.right() - x
            } else {
                width
            };
            (Column::ALL[index], Rect::new(x, area.y, w, area.height))
        })
        .collect()
}

pub(crate) fn board_rect(area: Rect, view: &BoardView) -> Rect {
    let goals_height = goals_height(view, area.height);
    Rect::new(
        area.x,
        area.y.saturating_add(1 + goals_height),
        area.width,
        area.height.saturating_sub(2 + goals_height),
    )
}

fn render_columns(app: &AppState, view: &BoardView, area: Rect, frame: &mut Frame) {
    for (column, rect) in column_rects(area, view.column) {
        if rect.width < 2 || rect.height < 2 {
            continue;
        }
        let selected = column == view.column;
        let style = Style::default().fg(if selected {
            app.palette.accent
        } else {
            app.palette.surface_dim
        });
        let count = view
            .board
            .cards
            .iter()
            .filter(|card| card.column == column)
            .count();
        let title = format!(" {} · {count} ", column.label());
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(style);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if column == Column::Draft && rect.width >= 8 {
            frame.render_widget(
                Paragraph::new("+"),
                Rect::new(rect.right() - 3, rect.y, 1, 1),
            );
        }
        let mut cards: Vec<_> = view
            .board
            .cards
            .iter()
            .filter(|card| card.column == column)
            .collect();
        if column == Column::InProgress {
            cards.sort_by_key(|card| app.board_lane(card));
        }
        let mut y = inner.y;
        let lanes = [Lane::Blocked, Lane::Working, Lane::DoneAwaitingYou];
        let mut lane_index = 0;
        let start = if selected { view.row } else { 0 };
        for (index, card) in cards.into_iter().enumerate().skip(start) {
            if y >= inner.bottom() {
                break;
            }
            if column == Column::InProgress {
                let lane = app.board_lane(card);
                while lane_index < lanes.len() && lanes[lane_index] <= lane && y < inner.bottom() {
                    let current = lanes[lane_index];
                    let marker = match current {
                        Lane::Blocked => "●",
                        Lane::Working => "◐",
                        Lane::DoneAwaitingYou => "✓",
                    };
                    frame.render_widget(
                        Paragraph::new(format!(" {marker} {}", current.label()))
                            .style(Style::default().fg(app.palette.subtext0)),
                        Rect::new(inner.x, y, inner.width, 1),
                    );
                    y += 1;
                    lane_index += 1;
                }
            }
            if y >= inner.bottom() {
                break;
            }
            let height = u16::try_from(card.agents.len())
                .unwrap_or(u16::MAX)
                .saturating_add(4)
                .min(inner.bottom() - y);
            let card_rect = Rect::new(inner.x, y, inner.width, height);
            let selected_card = selected && view.row == index;
            let text_width = usize::from(card_rect.width.saturating_sub(2));
            let mut lines = vec![
                format!(
                    "{} {}",
                    if selected_card { "▸" } else { " " },
                    super::text::truncate_end(&card.title, text_width.saturating_sub(2))
                ),
                format!(
                    " {}{}",
                    card.area.label(),
                    if card.agents.is_empty() && column != Column::Draft {
                        " · ▶ spawn"
                    } else {
                        ""
                    }
                ),
            ];
            for agent in &card.agents {
                let info = app.board_agent(agent);
                let dot = match info.lane {
                    Lane::Blocked => "●",
                    Lane::Working => "◐",
                    Lane::DoneAwaitingYou => "✓",
                };
                lines.push(super::text::truncate_end(
                    &format!(" {dot} {} {}", info.host, info.last_line),
                    text_width,
                ));
            }
            frame.render_widget(
                Paragraph::new(lines.join("\n"))
                    .block(Block::default().borders(Borders::ALL))
                    .style(Style::default().fg(if selected_card {
                        app.palette.accent
                    } else {
                        app.palette.text
                    })),
                card_rect,
            );
            y += height;
        }
        if column == Column::InProgress {
            while lane_index < lanes.len() && y < inner.bottom() {
                let lane = lanes[lane_index];
                let marker = match lane {
                    Lane::Blocked => "●",
                    Lane::Working => "◐",
                    Lane::DoneAwaitingYou => "✓",
                };
                frame.render_widget(
                    Paragraph::new(format!(" {marker} {}", lane.label()))
                        .style(Style::default().fg(app.palette.subtext0)),
                    Rect::new(inner.x, y, inner.width, 1),
                );
                y += 1;
                lane_index += 1;
            }
        }
        if column == Column::Draft && count == 0 && inner.height > 1 {
            frame.render_widget(
                Paragraph::new(" + new to-do\n agents never pick up Draft")
                    .style(Style::default().fg(app.palette.subtext0)),
                inner,
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoardHit {
    NewGoal,
    GoalPage(i8),
    NewCard,
    Column(Column),
    Card {
        id: String,
        spawn: bool,
        agent: Option<usize>,
    },
    DialogSave,
    DialogCancel,
    DialogField(usize),
    EditorSave,
    EditorCancel,
    DetailTab(bool),
    DetailAgent(usize),
    DetailEdit,
    DetailAppend,
}

pub(crate) fn hit_at(app: &AppState, area: Rect, x: u16, y: u16) -> Option<BoardHit> {
    let view = app.board_view.as_ref()?;
    if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
        return None;
    }
    if view.editor.is_some() {
        let rect = side_rect(area);
        if x < rect.x {
            return None;
        }
        if y == rect.bottom().saturating_sub(2) {
            return Some(if x < rect.x + 12 {
                BoardHit::EditorSave
            } else {
                BoardHit::EditorCancel
            });
        }
        if y == rect.y {
            return Some(BoardHit::EditorCancel);
        }
        return None;
    }
    if view.dialog.is_some() {
        let rect = side_rect(area);
        if x < rect.x {
            return None;
        }
        if y == rect.bottom().saturating_sub(2) {
            return if x < rect.x + 11 {
                Some(BoardHit::DialogSave)
            } else {
                Some(BoardHit::DialogCancel)
            };
        }
        if y == rect.y {
            return Some(BoardHit::DialogCancel);
        }
        let field = match view.dialog.as_ref()? {
            Dialog::Card { description, .. } => {
                let desc_lines = description.lines().count().max(1) as u16;
                if y <= rect.y + 3 {
                    Some(0)
                } else if y >= rect.y + 5 && y <= rect.y + 5 + desc_lines {
                    Some(1)
                } else if y == rect.y + 7 + desc_lines {
                    Some(2)
                } else if y == rect.y + 8 + desc_lines {
                    Some(3)
                } else if y == rect.y + 9 + desc_lines {
                    Some(4)
                } else {
                    None
                }
            }
            Dialog::Goal { .. } => {
                if y <= rect.y + 3 {
                    Some(0)
                } else if y == rect.y + 5 {
                    Some(1)
                } else if y >= rect.y + 8 {
                    Some(2)
                } else {
                    None
                }
            }
        };
        return field.map(BoardHit::DialogField);
    }
    if let Some(detail) = &view.detail {
        let rect = side_rect(area);
        if x < rect.x {
            return None;
        }
        if y == rect.bottom().saturating_sub(2) {
            let second_button = rect.x + if detail.agent_tab { 15 } else { 12 };
            return Some(if x < second_button {
                BoardHit::DetailEdit
            } else {
                BoardHit::DetailAppend
            });
        }
        if y == rect.y + 1 {
            return Some(BoardHit::DetailTab(x >= rect.x + 12));
        }
        if let Some(card) = view.board.card(&detail.card_id) {
            if let Some((first, list)) =
                detail_agent_rows(rect, detail.agent_row, card.agents.len())
            {
                if y >= list.y && y < list.bottom() {
                    return Some(BoardHit::DetailAgent(first + usize::from(y - list.y)));
                }
            }
        }
        return None;
    }
    if y == area.y && x >= area.right().saturating_sub(8) {
        return Some(BoardHit::NewGoal);
    }
    if y == area.y && area.width > 26 && view.board.goals.len() > goal_page_size(area.width) {
        if x == area.right().saturating_sub(13) {
            return Some(BoardHit::GoalPage(-1));
        }
        if x == area.right().saturating_sub(11) {
            return Some(BoardHit::GoalPage(1));
        }
    }
    let board = board_rect(area, view);
    if y < board.y || y >= board.bottom() {
        return None;
    }
    for (column, rect) in column_rects(board, view.column) {
        if x < rect.x || x >= rect.right() {
            continue;
        }
        if y == rect.y {
            return if column == Column::Draft && x >= rect.right().saturating_sub(4) {
                Some(BoardHit::NewCard)
            } else {
                Some(BoardHit::Column(column))
            };
        }
        let inner = Rect::new(
            rect.x + 1,
            rect.y + 1,
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );
        let mut cards: Vec<_> = view
            .board
            .cards
            .iter()
            .filter(|card| card.column == column)
            .collect();
        if column == Column::InProgress {
            cards.sort_by_key(|card| app.board_lane(card));
        }
        let start = if column == view.column { view.row } else { 0 };
        let mut top = inner.y;
        let lanes = [Lane::Blocked, Lane::Working, Lane::DoneAwaitingYou];
        let mut lane_index = 0;
        for card in cards.into_iter().skip(start) {
            if column == Column::InProgress {
                let lane = app.board_lane(card);
                while lane_index < lanes.len() && lanes[lane_index] <= lane {
                    top += 1;
                    lane_index += 1;
                }
            }
            if top >= inner.bottom() {
                break;
            }
            let height = u16::try_from(card.agents.len())
                .unwrap_or(u16::MAX)
                .saturating_add(4)
                .min(inner.bottom().saturating_sub(top));
            if y >= top && y < top + height {
                let offset = y - top;
                return Some(BoardHit::Card {
                    id: card.id.clone(),
                    spawn: offset == 2 && card.agents.is_empty() && column != Column::Draft,
                    agent: (offset >= 3 && usize::from(offset - 3) < card.agents.len())
                        .then_some(usize::from(offset.saturating_sub(3))),
                });
            }
            top += height;
            if top >= inner.bottom() {
                break;
            }
        }
        return if column == Column::Draft {
            Some(BoardHit::NewCard)
        } else {
            Some(BoardHit::Column(column))
        };
    }
    None
}

pub(crate) fn side_rect(area: Rect) -> Rect {
    let width = area.width.clamp(36, 70).min(area.width);
    Rect::new(
        area.right() - width,
        area.y,
        width,
        area.height.saturating_sub(1),
    )
}

fn render_detail(
    app: &AppState,
    view: &BoardView,
    detail: &crate::board::Detail,
    area: Rect,
    frame: &mut Frame,
) {
    let Some(card) = view.board.card(&detail.card_id) else {
        return;
    };
    let rect = side_rect(area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} · {} · Esc ", card.title, card.area.label()))
            .style(
                Style::default()
                    .bg(app.palette.panel_bg)
                    .fg(app.palette.text),
            ),
        rect,
    );
    let agent_rows = detail_agent_rows(rect, detail.agent_row, card.agents.len());
    let body_bottom = agent_rows
        .map(|(_, list)| list.y.saturating_sub(1))
        .unwrap_or(rect.bottom().saturating_sub(2));
    let mut lines = vec![
        format!(
            " {} Human   {} Agent",
            if detail.agent_tab { "" } else { "▸" },
            if detail.agent_tab { "▸" } else { "" }
        ),
        String::new(),
    ];
    if detail.agent_tab {
        lines.push(format!("Summary: {}", card.agent_summary));
        for update in &card.updates {
            lines.push(format!("• {}", update.text));
        }
    } else {
        lines.push(card.description.clone());
    }
    frame.render_widget(
        Paragraph::new(lines.join("\n")).wrap(Wrap { trim: false }),
        Rect::new(
            rect.x + 1,
            rect.y + 1,
            rect.width.saturating_sub(2),
            body_bottom.saturating_sub(rect.y + 1),
        ),
    );
    if let Some((first, list)) = agent_rows {
        frame.render_widget(
            Paragraph::new("Terminals · Enter to jump"),
            Rect::new(list.x, list.y.saturating_sub(1), list.width, 1),
        );
        let mut agent_lines = Vec::new();
        for (i, agent) in card
            .agents
            .iter()
            .enumerate()
            .skip(first)
            .take(usize::from(list.height))
        {
            let info = app.board_agent(agent);
            agent_lines.push(format!(
                "{} {} · {} · {}",
                if i == detail.agent_row { "▸" } else { " " },
                info.host,
                info.pane_id,
                info.last_line
            ));
        }
        frame.render_widget(Paragraph::new(agent_lines.join("\n")), list);
    }
    let actions = if detail.agent_tab {
        "[ Summary ]  [ Add update ]"
    } else {
        "[ Edit ]  [ Append ]"
    };
    frame.render_widget(
        Paragraph::new(actions).style(Style::default().fg(app.palette.accent)),
        Rect::new(
            rect.x + 2,
            rect.bottom().saturating_sub(2),
            rect.width.saturating_sub(4),
            1,
        ),
    );
}

fn detail_agent_rows(rect: Rect, selected: usize, count: usize) -> Option<(usize, Rect)> {
    let shown = count.min(usize::from(rect.height.saturating_sub(7)));
    if shown == 0 {
        return None;
    }
    let first = selected
        .saturating_add(1)
        .saturating_sub(shown)
        .min(count - shown);
    let height = shown as u16;
    Some((
        first,
        Rect::new(
            rect.x + 2,
            rect.bottom().saturating_sub(2 + height),
            rect.width.saturating_sub(4),
            height,
        ),
    ))
}

fn render_dialog(app: &AppState, dialog: &Dialog, area: Rect, frame: &mut Frame) {
    let rect = side_rect(area);
    let title = match dialog {
        Dialog::Card { .. } => " New to-do → Draft ",
        Dialog::Goal { .. } => " New goal ",
    };
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(title).style(
            Style::default()
                .bg(app.palette.panel_bg)
                .fg(app.palette.text),
        ),
        rect,
    );
    let goal_label = match dialog {
        Dialog::Card { goal: Some(id), .. } => app
            .board_view
            .as_ref()
            .and_then(|view| view.board.goals.iter().find(|goal| goal.id == *id))
            .map(|goal| goal.title.as_str())
            .unwrap_or("none"),
        _ => "none",
    };
    let text = match dialog {
        Dialog::Card { title, description, area, new_goal, field, .. } => format!(
            "{} Title\n{}\n\n{} Description\n{}\n\n{} Area: {}\n{} Goal: {}\n{} New goal title: {}\n\nCtrl+Enter Save to Draft",
            marker(*field, 0), title, marker(*field, 1), description,
            marker(*field, 2), area.label(), marker(*field, 3), goal_label,
            marker(*field, 4), new_goal),
        Dialog::Goal { title, scope, todos, field } => format!(
            "{} Title\n{}\n\n{} Scope: {}\n\n{} To-dos (one per line)\n{}\n\nCtrl+Enter Create goal",
            marker(*field, 0), title, marker(*field, 1), scope.label(), marker(*field, 2), todos),
    };
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }),
        Rect::new(
            rect.x + 2,
            rect.y + 2,
            rect.width.saturating_sub(4),
            rect.height.saturating_sub(4),
        ),
    );
    frame.render_widget(
        Paragraph::new("[ Save ]  [ Cancel ]").style(Style::default().fg(app.palette.accent)),
        Rect::new(
            rect.x + 2,
            rect.bottom().saturating_sub(2),
            rect.width.saturating_sub(4),
            1,
        ),
    );
}

fn marker(field: usize, current: usize) -> &'static str {
    if field == current {
        "▸"
    } else {
        " "
    }
}

fn render_editor(app: &AppState, editor: &crate::board::Editor, area: Rect, frame: &mut Frame) {
    let rect = side_rect(area);
    let title = match editor.field {
        EditField::HumanReplace => " Edit Human text ",
        EditField::HumanAppend => " Append Human text ",
        EditField::AgentSummary => " Edit Agent summary ",
        EditField::AgentUpdate => " Append Agent update ",
    };
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(title).style(
            Style::default()
                .bg(app.palette.panel_bg)
                .fg(app.palette.text),
        ),
        rect,
    );
    frame.render_widget(
        Paragraph::new(editor.text.as_str()).wrap(Wrap { trim: false }),
        Rect::new(
            rect.x + 2,
            rect.y + 2,
            rect.width.saturating_sub(4),
            rect.height.saturating_sub(4),
        ),
    );
    frame.render_widget(
        Paragraph::new("[ Save ]  [ Cancel ]").style(Style::default().fg(app.palette.accent)),
        Rect::new(
            rect.x + 2,
            rect.bottom().saturating_sub(2),
            rect.width.saturating_sub(4),
            1,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Area, Board, Card, Goal, GoalScope, WeekNote};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn wide_board_renders_goal_strip_columns_and_working_lane() {
        let date = time::Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let note = WeekNote::for_date(std::path::Path::new("/vault"), date).expect("note");
        let mut app = AppState::test_new();
        app.board_view = Some(BoardView::test_new(
            note,
            Board {
                goals: vec![Goal {
                    id: "g1".into(),
                    title: "Board v1 shipped".into(),
                    scope: GoalScope::Week,
                }],
                cards: vec![Card {
                    id: "c1".into(),
                    title: "goals sync".into(),
                    description: "raw".into(),
                    area: Area::Personal,
                    column: Column::InProgress,
                    goal_id: Some("g1".into()),
                    agent_summary: String::new(),
                    updates: Vec::new(),
                    agents: vec![crate::board::AgentLink {
                        host: "ub1".into(),
                        pane_id: "w:p1".into(),
                    }],
                }],
            },
        ));
        app.board_view.as_mut().expect("board").agent_lines.insert(
            crate::board::AgentLink {
                host: "ub1".into(),
                pane_id: "w:p1".into(),
            },
            (1, "last words".into()),
        );
        let width = 120;
        let backend = TestBackend::new(width, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render(&app, frame.area(), frame))
            .expect("board render");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        for required in [
            "Mo 28.09 · W40 28.09–04.10",
            "Board v1 shipped",
            "Draft",
            "To Do",
            "In Progress",
            "Done",
            "working",
            "goals sync",
            "ub1 last words",
        ] {
            assert!(text.contains(required), "missing {required}: {text}");
        }
    }

    #[test]
    fn narrow_board_pages_columns_without_losing_order() {
        let area = Rect::new(20, 5, 60, 18);
        assert_eq!(
            column_rects(area, Column::Draft)
                .iter()
                .map(|(column, _)| *column)
                .collect::<Vec<_>>(),
            vec![Column::Draft, Column::Todo]
        );
        assert_eq!(
            column_rects(area, Column::Done)
                .iter()
                .map(|(column, _)| *column)
                .collect::<Vec<_>>(),
            vec![Column::InProgress, Column::Done]
        );
        assert_eq!(goal_page_size(120), 4);
        assert_eq!(goal_page_size(60), 2);
    }

    #[test]
    fn mouse_hits_draft_dialog_fields_and_card_terminal_line() {
        let date = time::Date::from_calendar_date(2026, time::Month::September, 28).expect("date");
        let note = WeekNote::for_date(std::path::Path::new("/vault"), date).expect("note");
        let mut app = AppState::test_new();
        app.board_view = Some(BoardView::test_new(
            note,
            Board {
                goals: Vec::new(),
                cards: vec![Card {
                    id: "c1".into(),
                    title: "Run test".into(),
                    description: String::new(),
                    area: Area::Harness,
                    column: Column::Todo,
                    goal_id: None,
                    agent_summary: String::new(),
                    updates: Vec::new(),
                    agents: vec![crate::board::AgentLink {
                        host: "ub2".into(),
                        pane_id: "w:p1".into(),
                    }],
                }],
            },
        ));
        let area = Rect::new(0, 0, 120, 30);
        let board = board_rect(area, app.board_view.as_ref().expect("view"));
        let todo = column_rects(board, Column::Draft)[1].1;
        assert!(matches!(
            hit_at(&app, area, todo.x + 2, todo.y + 4),
            Some(BoardHit::Card { agent: Some(0), .. })
        ));
        app.board_view.as_mut().expect("view").dialog = Some(Dialog::Card {
            title: String::new(),
            description: String::new(),
            area: Area::Harness,
            goal: None,
            new_goal: String::new(),
            field: 0,
        });
        let side = side_rect(area);
        assert_eq!(
            hit_at(&app, area, side.x + 3, side.y + 8),
            Some(BoardHit::DialogField(2))
        );
        let view = app.board_view.as_mut().expect("view");
        view.dialog = None;
        view.detail = Some(crate::board::Detail {
            card_id: "c1".into(),
            agent_tab: false,
            agent_row: 0,
        });
        assert_eq!(
            hit_at(&app, area, side.x + 3, side.bottom() - 2),
            Some(BoardHit::DetailEdit)
        );
        assert_eq!(
            hit_at(&app, area, side.x + 15, side.bottom() - 2),
            Some(BoardHit::DetailAppend)
        );
        assert_eq!(
            hit_at(&app, area, side.x + 15, side.y + 1),
            Some(BoardHit::DetailTab(true))
        );
        app.board_view.as_mut().expect("view").board.cards[0].description =
            "long description ".repeat(30);
        assert_eq!(
            hit_at(&app, area, side.x + 3, side.bottom() - 3),
            Some(BoardHit::DetailAgent(0))
        );
        app.board_view.as_mut().expect("view").detail = None;
        app.board_view.as_mut().expect("view").dialog = Some(Dialog::Goal {
            title: String::new(),
            scope: GoalScope::Week,
            todos: String::new(),
            field: 0,
        });
        assert_eq!(
            hit_at(&app, area, side.x + 3, side.y + 5),
            Some(BoardHit::DialogField(1))
        );
        app.board_view.as_mut().expect("view").dialog = None;
        app.board_view.as_mut().expect("view").editor = Some(crate::board::Editor {
            field: EditField::HumanReplace,
            text: String::new(),
        });
        assert_eq!(
            hit_at(&app, area, side.x + 15, side.bottom() - 2),
            Some(BoardHit::EditorCancel)
        );
    }
}
