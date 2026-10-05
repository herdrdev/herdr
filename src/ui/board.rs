use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::state::AppState,
    board::{BoardView, Column, Dialog, EditField, GoalScope, Lane},
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
    render_goals(app, view, Rect::new(area.x, area.y, area.width, 1), frame);
    let board_area = board_rect(area, view);
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

fn header_date(view: &BoardView) -> String {
    let (_, week, _) = view.note.today.to_iso_week_date();
    let date = view.note.header();
    format!("W{week:02} · {}", date.split(" · ").next().unwrap_or(""))
}

fn goal_area(area: Rect, view: &BoardView) -> Rect {
    let start = super::text::display_width_u16(&header_date(view))
        .saturating_add(4)
        .min(area.width);
    Rect::new(
        area.x + start,
        area.y,
        area.width.saturating_sub(start + 8),
        1,
    )
}

fn new_goal_rect(area: Rect) -> Rect {
    Rect::new(
        area.right().saturating_sub(2).max(area.x),
        area.y,
        area.width.min(2),
        1,
    )
}

fn render_goals(app: &AppState, view: &BoardView, area: Rect, frame: &mut Frame) {
    frame.render_widget(
        Paragraph::new(format!("  {}", header_date(view))).style(
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
    frame.render_widget(
        Paragraph::new("＋").style(Style::default().fg(app.palette.accent)),
        new_goal_rect(area),
    );
    let goals = goal_area(area, view);
    let count = goal_page_size(goals.width);
    if view.board.goals.len() > count {
        frame.render_widget(
            Paragraph::new("← →"),
            Rect::new(area.right().saturating_sub(7), area.y, 3, 1),
        );
    }
    let width = goals.width / count as u16;
    for (i, goal) in view
        .board
        .goals
        .iter()
        .skip(view.goal_offset)
        .take(count)
        .enumerate()
    {
        let total = view
            .board
            .cards
            .iter()
            .filter(|card| card.goal_id.as_deref() == Some(&goal.id))
            .count();
        let done = view
            .board
            .cards
            .iter()
            .filter(|card| card.goal_id.as_deref() == Some(&goal.id) && card.column == Column::Done)
            .count();
        let bars = total.clamp(1, 8);
        let filled = (done * bars).checked_div(total).unwrap_or(0);
        let progress = format!(
            " {}{} {done}/{total}",
            "▰".repeat(filled),
            "▱".repeat(bars - filled)
        );
        let scope = if goal.scope == GoalScope::Week {
            String::new()
        } else {
            format!(" {}", goal.scope.label())
        };
        let reserved = super::text::display_width_u16(&progress)
            .saturating_add(super::text::display_width_u16(&scope))
            .saturating_add(4);
        let title =
            super::text::truncate_end(&goal.title, usize::from(width.saturating_sub(reserved)));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(format!("◎ {title}")),
                Span::raw(progress),
                Span::styled(scope, Style::default().fg(app.palette.subtext0)),
            ]))
            .style(Style::default().fg(app.palette.text)),
            Rect::new(goals.x + i as u16 * width, goals.y, width, 1),
        );
    }
}

pub(crate) fn goal_page_capacity(area: Rect, view: &BoardView) -> usize {
    goal_page_size(goal_area(area, view).width)
}

pub(crate) fn goal_page_size(width: u16) -> usize {
    usize::from((width / 32).clamp(1, 4))
}

pub(crate) fn column_rects(area: Rect, selected: Column) -> Vec<(Column, Rect)> {
    if area.width == 0 {
        return Vec::new();
    }
    let columns: &[Column] = if area.width >= 100 {
        &Column::ALL
    } else if matches!(selected, Column::Draft | Column::Todo) {
        &Column::ALL[..2]
    } else {
        &Column::ALL[2..]
    };
    let collapsed = selected != Column::Done && columns.contains(&Column::Done);
    let strip = if collapsed { 7.min(area.width) } else { 0 };
    let available = area.width - strip;
    let weight = |column| match column {
        Column::Draft => 2,
        Column::Todo => 3,
        Column::InProgress => 5,
        Column::Done => 3,
    };
    let total: u16 = columns
        .iter()
        .filter(|c| !(collapsed && **c == Column::Done))
        .map(|c| weight(*c))
        .sum();
    let mut x = area.x;
    columns
        .iter()
        .enumerate()
        .map(|(i, &column)| {
            let width = if collapsed && column == Column::InProgress {
                area.right() - x - strip
            } else if i + 1 == columns.len() {
                area.right() - x
            } else if collapsed && column == Column::Done {
                strip
            } else {
                (u32::from(available) * u32::from(weight(column)) / u32::from(total)) as u16
            };
            let rect = Rect::new(x, area.y, width, area.height);
            x += width;
            (column, rect)
        })
        .collect()
}

pub(crate) fn board_rect(area: Rect, _view: &BoardView) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(2),
    )
}

fn ordered_cards<'a>(
    app: &AppState,
    view: &'a BoardView,
    column: Column,
) -> Vec<&'a crate::board::Card> {
    let mut cards: Vec<_> = view
        .board
        .cards
        .iter()
        .filter(|card| card.column == column)
        .collect();
    if column == Column::InProgress {
        cards.sort_by_key(|card| app.board_lane(card));
    }
    cards
}

fn card_agent_metadata(
    app: &AppState,
    view: &BoardView,
    card: &crate::board::Card,
    row: Rect,
) -> Option<(usize, Rect, String)> {
    if card.column != Column::InProgress {
        return None;
    }
    let (index, agent) = card
        .agents
        .iter()
        .enumerate()
        .min_by_key(|(_, agent)| app.board_agent(agent).lane)?;
    let age = view
        .agent_activity
        .get(agent)
        .map(|at| crate::activity_age::coarse_label(Some(*at), app.view_observed_at));
    let label = match age {
        Some(age) => format!("{} · {age}", agent.host),
        None => agent.host.clone(),
    };
    let width = super::text::display_width_u16(&label);
    // Metadata yields first on narrow screens, preserving a readable title.
    if width == 0 || row.width <= width.saturating_add(12) {
        return None;
    }
    Some((
        index,
        Rect::new(row.right() - width, row.y, width, 1),
        label,
    ))
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
        let collapsed = column == Column::Done && !selected;
        let title = if collapsed {
            format!(" ✓ {count} ")
        } else {
            format!(" {} {count} ", column.label())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(style);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if column == Column::Draft && rect.width >= 8 {
            frame.render_widget(
                Paragraph::new("＋"),
                Rect::new(rect.right() - 3, rect.y, 2, 1),
            );
        }
        if collapsed {
            continue;
        }
        let start = if selected { view.row } else { 0 };
        for (offset, card) in ordered_cards(app, view, column)
            .into_iter()
            .skip(start)
            .take(usize::from(inner.height))
            .enumerate()
        {
            let selected_card = selected && offset == 0;
            let lane = app.board_lane(card);
            let prefix = if column == Column::InProgress {
                format!("{} ", lane.glyph())
            } else {
                String::new()
            };
            let suffix = if card.goal_id.is_some() { " ◎" } else { "" };
            let y = inner.y + offset as u16;
            let metadata =
                card_agent_metadata(app, view, card, Rect::new(inner.x, y, inner.width, 1));
            let text_width = metadata
                .as_ref()
                .map(|(_, rect, _)| rect.x.saturating_sub(inner.x + 2))
                .unwrap_or(inner.width);
            let icon = card.area.icon(app.nerd_font);
            let reserved = super::text::display_width_u16(&format!(" {prefix}{icon} {suffix}"));
            let title = super::text::truncate_end(
                &card.title,
                usize::from(text_width.saturating_sub(reserved)),
            );
            let lane_color = match lane {
                Lane::Blocked => app.palette.red,
                Lane::Working => app.palette.green,
                Lane::DoneAwaitingYou => app.palette.accent,
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(prefix, Style::default().fg(lane_color)),
                    Span::raw(format!("{icon} {title}{suffix}")),
                ]))
                .style(
                    Style::default()
                        .fg(if selected_card {
                            app.palette.accent
                        } else {
                            app.palette.text
                        })
                        .add_modifier(if selected_card {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Rect::new(inner.x, y, text_width, 1),
            );
            if let Some((_, rect, label)) = metadata {
                frame.render_widget(
                    Paragraph::new(label).style(Style::default().fg(app.palette.subtext0)),
                    rect,
                );
            }
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
    if contains(new_goal_rect(area), x, y) {
        return Some(BoardHit::NewGoal);
    }
    if y == area.y && view.board.goals.len() > goal_page_capacity(area, view) {
        if x == area.right().saturating_sub(7) {
            return Some(BoardHit::GoalPage(-1));
        }
        if x == area.right().saturating_sub(5) {
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
        if column == Column::Done && view.column != Column::Done {
            return Some(BoardHit::Column(Column::Done));
        }
        let start = if column == view.column { view.row } else { 0 };
        if contains(inner, x, y) {
            if let Some(card) =
                ordered_cards(app, view, column).get(start + usize::from(y - inner.y))
            {
                return Some(BoardHit::Card {
                    id: card.id.clone(),
                    spawn: false,
                    agent: card_agent_metadata(
                        app,
                        view,
                        card,
                        Rect::new(inner.x, y, inner.width, 1),
                    )
                    .and_then(|(index, rect, _)| contains(rect, x, y).then_some(index)),
                });
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

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

pub(crate) fn hovered_control(
    app: &AppState,
    x: u16,
    y: u16,
) -> Option<crate::app::state::ControlId> {
    use crate::app::state::ControlId;
    let view = app.board_view.as_ref()?;
    if view.dialog.is_some() || view.detail.is_some() || view.editor.is_some() {
        return None;
    }
    match hit_at(app, app.view.terminal_area, x, y)? {
        BoardHit::NewGoal => Some(ControlId::FocusBoardNewGoal),
        BoardHit::NewCard => Some(ControlId::FocusBoardNewCard),
        BoardHit::Column(Column::Done) => Some(ControlId::FocusBoardDone),
        BoardHit::Card { id, .. } => view
            .board
            .cards
            .iter()
            .position(|card| card.id == id)
            .map(ControlId::FocusBoardCard),
        _ => None,
    }
}

pub(crate) fn tooltip_target(
    app: &AppState,
    control: crate::app::state::ControlId,
) -> Option<(Rect, String)> {
    use crate::app::state::ControlId;
    let view = app.board_view.as_ref()?;
    if view.dialog.is_some() || view.detail.is_some() || view.editor.is_some() {
        return None;
    }
    let area = app.view.terminal_area;
    let columns = column_rects(board_rect(area, view), view.column);
    match control {
        ControlId::FocusBoardNewGoal => Some((new_goal_rect(area), "Add a goal".into())),
        ControlId::FocusBoardNewCard => {
            let (_, rect) = columns
                .iter()
                .find(|(column, _)| *column == Column::Draft)?;
            Some((
                Rect::new(rect.right().saturating_sub(3), rect.y, 2, 1),
                "Add a to-do · agents never pick up Draft".into(),
            ))
        }
        ControlId::FocusBoardDone => {
            let (_, rect) = columns.iter().find(|(column, _)| *column == Column::Done)?;
            Some((*rect, "Done · select to expand".into()))
        }
        ControlId::FocusBoardCard(index) => {
            let card = view.board.cards.get(index)?;
            let (_, rect) = columns.iter().find(|(column, _)| *column == card.column)?;
            if card.column == Column::Done && view.column != Column::Done {
                return None;
            }
            let position = ordered_cards(app, view, card.column)
                .iter()
                .position(|c| c.id == card.id)?;
            let offset = position.checked_sub(if view.column == card.column {
                view.row
            } else {
                0
            })?;
            if offset >= usize::from(rect.height.saturating_sub(2)) {
                return None;
            }
            let lane = if card.column == Column::InProgress {
                format!(" · {}", app.board_lane(card).label())
            } else {
                String::new()
            };
            Some((
                Rect::new(
                    rect.x + 1,
                    rect.y + 1 + offset as u16,
                    rect.width.saturating_sub(2),
                    1,
                ),
                format!("{} · {}{lane}", card.area.label(), card.title),
            ))
        }
        _ => None,
    }
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

    fn sample_card(id: &str, column: Column) -> Card {
        Card {
            id: id.into(),
            title: format!("Task {id}"),
            description: String::new(),
            area: Area::Personal,
            column,
            goal_id: None,
            agent_summary: String::new(),
            updates: Vec::new(),
            agents: Vec::new(),
        }
    }

    fn sample_app(cards: Vec<Card>) -> AppState {
        let date = time::Date::from_calendar_date(2026, time::Month::October, 5).expect("date");
        let mut app = AppState::test_new();
        app.board_view = Some(BoardView::test_new(
            WeekNote::for_date(std::path::Path::new("/vault"), date).expect("note"),
            Board {
                cards,
                goals: Vec::new(),
            },
        ));
        app.view.terminal_area = Rect::new(10, 3, 120, 25);
        app
    }

    fn rendered_text(app: &AppState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).expect("terminal");
        terminal
            .draw(|frame| render(app, frame.area(), frame))
            .expect("render");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(120)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn column_widths_prioritize_progress_and_expand_done_without_gaps() {
        for width in [30, 60, 100, 120, 240] {
            let area = Rect::new(7, 4, width, 20);
            for selected in Column::ALL {
                let columns = column_rects(area, selected);
                assert_eq!(columns[0].1.x, area.x);
                assert_eq!(columns.last().expect("column").1.right(), area.right());
                for pair in columns.windows(2) {
                    assert_eq!(pair[0].1.right(), pair[1].1.x);
                }
                if width >= 100 {
                    assert!(columns[0].1.width < columns[1].1.width);
                    assert!(columns[2].1.width > columns[1].1.width);
                    assert_eq!(columns[3].1.width == 7, selected != Column::Done);
                }
            }
        }
    }

    #[test]
    fn done_strip_click_selects_column_and_reveals_cards() {
        let mut app = sample_app(vec![sample_card("finished", Column::Done)]);
        let area = app.view.terminal_area;
        let view = app.board_view.as_ref().expect("view");
        let strip = column_rects(board_rect(area, view), view.column)[3].1;
        assert_eq!(
            hit_at(&app, area, strip.x + 2, strip.y + 1),
            Some(BoardHit::Column(Column::Done))
        );
        assert!(!rendered_text(&app).contains("Task finished"));
        app.board_view.as_mut().expect("view").column = Column::Done;
        assert!(rendered_text(&app).contains("Task finished"));
        let expanded = column_rects(
            board_rect(area, app.board_view.as_ref().expect("view")),
            Column::Done,
        )[3]
        .1;
        assert!(expanded.width > strip.width);
        assert!(
            matches!(hit_at(&app, area, expanded.x + 2, expanded.y + 1), Some(BoardHit::Card { id, .. }) if id == "finished")
        );
    }

    #[test]
    fn single_rows_sort_by_lane_and_mouse_matches_keyboard_order() {
        let mut cards = Vec::new();
        for id in ["done", "working", "blocked", "blocked2"] {
            let mut card = sample_card(id, Column::InProgress);
            card.agents.push(crate::board::AgentLink {
                host: "ub2".into(),
                pane_id: id.into(),
            });
            cards.push(card);
        }
        let mut app = sample_app(cards);
        let view = app.board_view.as_mut().expect("view");
        view.column = Column::InProgress;
        for card in &view.board.cards {
            let lane = match card.id.as_str() {
                "done" => Lane::DoneAwaitingYou,
                "working" => Lane::Working,
                _ => Lane::Blocked,
            };
            view.agent_lanes.insert(card.agents[0].clone(), lane);
            view.agent_activity.insert(
                card.agents[0].clone(),
                app.view_observed_at - std::time::Duration::from_secs(1200),
            );
        }
        let area = app.view.terminal_area;
        let view = app.board_view.as_ref().expect("view");
        let progress = column_rects(board_rect(area, view), view.column)[2].1;
        let ids: Vec<_> = view
            .visible_cards(&app)
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["blocked", "blocked2", "working", "done"]);
        for (row, id) in ids.iter().enumerate() {
            assert!(
                matches!(hit_at(&app, area, progress.x + 2, progress.y + 1 + row as u16), Some(BoardHit::Card { id: hit, .. }) if hit == *id)
            );
        }
        let text = rendered_text(&app);
        assert!(text.contains("ub2 · 20m"));
        assert!(!text.contains("awaiting you"));
        assert_eq!(
            app.next_agent_activity_age_change(app.view_observed_at),
            Some(app.view_observed_at + std::time::Duration::from_secs(60))
        );
        app.board_view.as_mut().expect("view").row = 2;
        assert!(
            matches!(hit_at(&app, area, progress.x + 2, progress.y + 1), Some(BoardHit::Card { id, .. }) if id == "working")
        );
    }

    #[test]
    fn agent_metadata_hit_uses_the_displayed_worst_lane_link() {
        let mut card = sample_card("linked", Column::InProgress);
        card.agents = vec![
            crate::board::AgentLink {
                host: "ub1".into(),
                pane_id: "working".into(),
            },
            crate::board::AgentLink {
                host: "ub2".into(),
                pane_id: "blocked".into(),
            },
        ];
        let mut app = sample_app(vec![card]);
        let view = app.board_view.as_mut().expect("view");
        view.agent_lanes
            .insert(view.board.cards[0].agents[1].clone(), Lane::Blocked);
        let area = app.view.terminal_area;
        let view = app.board_view.as_ref().expect("view");
        let progress = column_rects(board_rect(area, view), view.column)[2].1;
        assert!(rendered_text(&app).contains("ub2"));
        assert!(matches!(
            hit_at(&app, area, progress.right() - 2, progress.y + 1),
            Some(BoardHit::Card { agent: Some(1), .. })
        ));
        assert!(matches!(
            hit_at(&app, area, progress.x + 2, progress.y + 1),
            Some(BoardHit::Card { agent: None, .. })
        ));
        let narrow = Rect::new(0, 0, 14, 1);
        assert!(card_agent_metadata(&app, view, &view.board.cards[0], narrow).is_none());
    }

    #[test]
    fn icons_fallback_goal_scope_and_add_tooltips() {
        let mut card = sample_card("tax", Column::Todo);
        card.goal_id = Some("taxes".into());
        let mut app = sample_app(vec![card]);
        let view = app.board_view.as_mut().expect("view");
        view.board.goals = vec![Goal {
            id: "taxes".into(),
            title: "Taxes".into(),
            scope: GoalScope::Today,
        }];
        app.nerd_font = false;
        let text = rendered_text(&app);
        assert!(text.contains("P Task tax ◎"));
        assert!(text.contains("◎ Taxes ▱ 0/1 today"));
        assert_eq!(Area::Scalable.icon(false), "S");
        assert_eq!(Area::Harness.icon(false), "H");
        let area = app.view.terminal_area;
        let view = app.board_view.as_ref().expect("view");
        let draft = column_rects(board_rect(area, view), Column::Draft)[0].1;
        assert_eq!(
            hit_at(&app, area, draft.right() - 2, draft.y),
            Some(BoardHit::NewCard)
        );
        assert_eq!(
            hit_at(&app, area, area.right() - 2, area.y),
            Some(BoardHit::NewGoal)
        );
        let control = hovered_control(&app, draft.right() - 2, draft.y).expect("control");
        assert!(tooltip_target(&app, control)
            .expect("tooltip")
            .1
            .contains("agents never pick up Draft"));
        app.board_view.as_mut().expect("view").board.goals[0].scope = GoalScope::Week;
        assert!(!rendered_text(&app).contains("0/1 week"));
    }

    #[test]
    fn goal_paging_hits_share_header_capacity() {
        let mut app = sample_app(Vec::new());
        let view = app.board_view.as_mut().expect("view");
        view.board.goals = (0..8)
            .map(|i| Goal {
                id: i.to_string(),
                title: format!("Goal {i}"),
                scope: GoalScope::Week,
            })
            .collect();
        let area = app.view.terminal_area;
        assert!(goal_page_capacity(area, view) < view.board.goals.len());
        assert_eq!(
            hit_at(&app, area, area.right() - 7, area.y),
            Some(BoardHit::GoalPage(-1))
        );
        assert_eq!(
            hit_at(&app, area, area.right() - 5, area.y),
            Some(BoardHit::GoalPage(1))
        );
        assert_eq!(hit_at(&app, area, area.x - 1, area.y), None);
    }

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
            "W40 · Mo 28.09",
            "Board v1 shipped",
            "Draft",
            "To Do",
            "In Progress",
            "✓ 0",
            "◐",
            "goals sync",
            "ub1",
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
        assert_eq!(goal_page_size(120), 3);
        assert_eq!(goal_page_size(60), 1);
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
            hit_at(&app, area, todo.x + 2, todo.y + 1),
            Some(BoardHit::Card { agent: None, .. })
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
