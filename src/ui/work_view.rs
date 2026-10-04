use unicode_width::UnicodeWidthStr;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::state::{
        AppState, ObjectViewState, Palette, PrDetailTab, TicketTransitionChoice, WorkProjection,
        WorkViewState,
    },
    ui::work_list_detail::{
        comment_body_lines, comment_header, comment_identity, sorted_filtered_conversations,
        sorted_filtered_prs, sorted_filtered_tickets, ConversationItem, TicketItem, WorkItem as _,
        WorkRow,
    },
    work_projection::{project_review_queue, WorkReviewQueueRow},
};

pub(crate) fn render(app: &AppState, area: Rect, frame: &mut Frame) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(state) = app.work_view.as_ref() else {
        return;
    };
    let palette = &app.palette;
    let sections = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    match state.projection {
        WorkProjection::PullRequests => render_pull_requests(app, state, sections[0], frame),
        WorkProjection::Tickets => render_tickets(app, state, sections[0], frame),
        WorkProjection::Missive => render_missive(app, state, sections[0], frame),
        WorkProjection::ReviewQueue => render_review_queue(palette, state, sections[0], frame),
        projection => render_placeholder(palette, projection, sections[0], frame),
    }
    render_footer(palette, state, sections[1], frame);
}

fn render_missive(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let palette = &app.palette;
    let observed_at = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.observed_at)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let items = state.snapshot.as_ref().map(|snapshot| {
        sorted_filtered_conversations(
            &snapshot.conversations,
            &state.search,
            !state.open_only,
            observed_at,
        )
    });
    let columns = if area.width >= 96 {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(area)
    } else {
        Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)]).split(area)
    };
    let refresh = if state.refreshing {
        " · refreshing…"
    } else {
        ""
    };
    let left = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Missive{refresh} "))
        .border_style(Style::default().fg(palette.accent));
    let left_inner = left.inner(columns[0]);
    frame.render_widget(left, columns[0]);
    let cursor = if state.search_focused { "▏" } else { "" };
    let query = if state.search.is_empty() {
        "search conversations"
    } else {
        &state.search
    };
    let filter = if state.open_only { "open" } else { "all" };
    let query_width = usize::from(left_inner.width).saturating_sub(filter.chars().count() + 9);
    let mut lines = vec![Line::styled(
        format!(" 🔍 {}{cursor}  ⚲ {filter}", fit_cell(query, query_width)),
        Style::default().fg(palette.subtext0),
    )];
    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        lines.push(Line::styled(message, Style::default().fg(palette.subtext0)));
        frame.render_widget(Paragraph::new(lines), left_inner);
        return;
    }
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.short_unavailable_reason(
            crate::work_index::WorkIndexSource::Missive,
            std::time::SystemTime::now(),
        )
    }) {
        lines.push(Line::styled(
            format!("Missive: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    let items = items.unwrap_or_default();
    let selected = state
        .selected_missive
        .as_deref()
        .and_then(|id| items.iter().position(|item| item.key() == id))
        .unwrap_or(0);
    let mut current_group = "";
    let mut selected_line = 0usize;
    for (index, item) in items.iter().enumerate() {
        let row = item.row();
        if row.group != current_group {
            current_group = row.group;
            lines.push(Line::styled(
                format!(" {current_group}"),
                Style::default()
                    .fg(palette.subtext0)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if index == selected {
            selected_line = lines.len();
        }
        push_missive_row(
            &mut lines,
            item,
            &row,
            index == selected,
            palette,
            left_inner.width,
        );
    }
    if items.is_empty() {
        lines.push(Line::styled(
            " no matching conversations",
            Style::default().fg(palette.subtext0),
        ));
    }
    let list_height = usize::from(left_inner.height);
    let list_scroll = selected_line
        .saturating_add(1)
        .saturating_sub(list_height)
        .min(usize::from(u16::MAX)) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((list_scroll, 0)), left_inner);

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(" Conversation ")
        .border_style(Style::default().fg(palette.accent));
    let detail_inner = detail_block.inner(columns[1]);
    frame.render_widget(detail_block, columns[1]);
    if let Some(item) = items.get(selected) {
        render_missive_detail(app, state, item, detail_inner, frame);
    } else if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.unavailable_reason(crate::work_index::WorkIndexSource::Missive)
    }) {
        frame.render_widget(Paragraph::new(format!(" details\n {reason}")), detail_inner);
    }
}

fn render_missive_detail(
    app: &AppState,
    state: &WorkViewState,
    item: &ConversationItem<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    let detail = item.detail();
    let mut lines = vec![
        Line::styled(
            format!(" {}   [Start thread ▾] [Open in Missive]", detail.heading),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.title),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.byline),
            Style::default().fg(app.palette.subtext0),
        ),
    ];
    let visible_sections = detail
        .sections
        .iter()
        .filter(|section| !section.entries.is_empty())
        .collect::<Vec<_>>();
    for section in &visible_sections {
        lines.push(Line::styled(
            format!(" {}  {}", section.label, section.entries.len()),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ));
        for entry in &section.entries {
            lines.push(Line::styled(
                format!(
                    "  {} · {}",
                    entry.author.as_deref().unwrap_or("unknown"),
                    relative_time(entry.created_at, item.observed_at)
                ),
                Style::default().fg(app.palette.subtext0),
            ));
            lines.extend(crate::ui::markdown::body_lines(
                &app.palette,
                Some(&entry.body),
                usize::from(area.width.saturating_sub(4)),
                "    ",
            ));
        }
    }
    if visible_sections.is_empty() {
        lines.push(Line::styled(
            " no conversation entries indexed",
            Style::default().fg(app.palette.subtext0),
        ));
    }
    if let Some(hint) = state.hint.as_deref() {
        lines.push(Line::styled(
            format!(" {hint}"),
            Style::default().fg(app.palette.accent),
        ));
    }
    let max_scroll = lines.len().saturating_sub(usize::from(area.height));
    let scroll = usize::from(state.missive_detail_scroll).min(max_scroll) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);

    if let Some(choice) = state.missive_start_menu {
        render_missive_start_menu(app, frame, area, choice);
    }
}

fn relative_time(then: Option<std::time::SystemTime>, now: std::time::SystemTime) -> String {
    let seconds = then
        .and_then(|then| now.duration_since(then).ok())
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    if seconds >= 86_400 {
        format!("{}d ago", seconds / 86_400)
    } else if seconds >= 3_600 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}m ago", seconds / 60)
    }
}

fn render_missive_start_menu(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    choice: crate::app::state::PrCheckoutChoice,
) {
    let selected = usize::from(choice == crate::app::state::PrCheckoutChoice::NewWorktree);
    let Some(layout) = ticket_menu_layout(area, 12, 0, 2, selected, 24) else {
        return;
    };
    let labels = ["Current checkout", "New worktree"];
    frame.render_widget(
        Paragraph::new(
            labels
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    Line::styled(
                        format!("{} {label}", if index == selected { "▸" } else { " " }),
                        Style::default()
                            .fg(app.palette.text)
                            .bg(app.palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        layout.rect,
    );
}

/// The two columns a work-view projection splits its area into: list on the
/// left, detail on the right, stacked instead when the terminal is narrow.
///
/// The input layer resolves the same rectangles to hit-test the detail, so this
/// is the one place that decides where the detail is drawn.
fn detail_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    if area.width >= 72 {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(area)
    } else {
        Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)]).split(area)
    }
}

/// The area inside the detail column's border, which is where a detail layout's
/// first line lands.
pub(crate) fn detail_inner_rect(area: Rect) -> Rect {
    Block::default()
        .borders(Borders::ALL)
        .inner(detail_columns(area)[1])
}

fn render_tickets(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    if state.ticket_layout == crate::app::state::LinearViewLayout::Board && !state.board_detail_open
    {
        render_ticket_board(app, state, area, frame);
        return;
    }
    let palette = &app.palette;
    let observed_at = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.observed_at)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let has_context_pr = super::dock::pr::focused_pr_key(app).is_some();
    let items = state.snapshot.as_ref().map(|snapshot| {
        if state.ticket_layout == crate::app::state::LinearViewLayout::Board {
            return ticket_board_items(app, state);
        }
        sorted_filtered_tickets(
            &snapshot.items,
            &app.work_item_detail_cache,
            &state.search,
            state.ticket_sort,
            state.ticket_open_only,
            observed_at,
            has_context_pr,
            Some((&app.sidebar_work_filter, &app.work_index_session)),
        )
    });
    let refresh = if state.refreshing || !app.work_item_detail_loading.is_empty() {
        " · refreshing…"
    } else {
        ""
    };
    let columns = detail_columns(area);
    let layout_label = match state.ticket_layout {
        crate::app::state::LinearViewLayout::List => "[List] Board",
        crate::app::state::LinearViewLayout::Board => "List [Board]",
    };
    let left = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Tickets{refresh} · {layout_label} "))
        .border_style(Style::default().fg(palette.accent));
    let left_inner = left.inner(columns[0]);
    frame.render_widget(left, columns[0]);
    let filter = if state.ticket_open_only {
        "open"
    } else {
        "all"
    };
    let cursor = if state.search_focused { "▏" } else { "" };
    let mut lines = vec![Line::styled(
        format!(
            " 🔍 {}{cursor}   ⇅ {}   ⚲ {filter}",
            if state.search.is_empty() {
                "search or label:bug"
            } else {
                &state.search
            },
            state.ticket_sort.label()
        ),
        Style::default().fg(palette.subtext0),
    )];
    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        lines.push(Line::styled(message, Style::default().fg(palette.subtext0)));
        frame.render_widget(Paragraph::new(lines), left_inner);
        return;
    }
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.short_unavailable_reason(
            crate::work_index::WorkIndexSource::Linear,
            std::time::SystemTime::now(),
        )
    }) {
        lines.push(Line::styled(
            format!("Linear: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    let items = items.unwrap_or_default();
    let selected = state
        .selected
        .as_ref()
        .and_then(|key| {
            key.ticket_id
                .as_deref()
                .and_then(|ticket| items.iter().position(|item| item.key() == ticket))
        })
        .unwrap_or(0);
    let mut current_group = "";
    for (index, item) in items.iter().enumerate() {
        let row = item.row();
        if row.group != current_group {
            current_group = row.group;
            lines.push(Line::styled(
                format!(" {current_group}"),
                Style::default()
                    .fg(palette.subtext0)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        push_work_row(
            &mut lines,
            &row,
            index == selected,
            palette,
            left_inner.width,
            false,
        );
    }
    if items.is_empty() {
        lines.push(Line::styled(
            " no matching tickets",
            Style::default().fg(palette.subtext0),
        ));
    }
    frame.render_widget(Paragraph::new(lines), left_inner);

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(" Ticket ")
        .border_style(Style::default().fg(palette.accent));
    let detail_inner = detail_block.inner(columns[1]);
    frame.render_widget(detail_block, columns[1]);
    if let Some(item) = items.get(selected) {
        let object_view = state
            .object_views
            .get(&item.stable_key())
            .cloned()
            .unwrap_or_default();
        render_ticket_detail(
            app,
            item,
            &object_view,
            TicketDetailControls {
                start_menu: state.ticket_start_menu,
                transition_menu: state.ticket_transition_menu,
                action_menu: state.ticket_more_menu,
                comment_draft: state.ticket_comment_draft.as_deref(),
                pending_write: state.pending_write.as_ref(),
                notice: state.hint.as_deref(),
            },
            detail_inner,
            frame,
        );
    } else if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.unavailable_reason(crate::work_index::WorkIndexSource::Linear)
    }) {
        frame.render_widget(Paragraph::new(format!(" details\n {reason}")), detail_inner);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinearBoardColumn {
    Triage,
    Todo,
    InProgress,
    InReview,
    Done,
}

impl LinearBoardColumn {
    pub(crate) const ALL: [Self; 5] = [
        Self::Triage,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Triage => "Triage",
            Self::Todo => "Backlog / Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
        }
    }

    pub(crate) fn for_ticket(ticket: &crate::work_index::WorkTicket) -> Self {
        let state = ticket.state.as_deref().unwrap_or_default().to_lowercase();
        if ticket.group == crate::work_index::TicketGroup::Triage || state.contains("triage") {
            Self::Triage
        } else if state.contains("done") || state.contains("complete") {
            Self::Done
        } else if state.contains("review") {
            Self::InReview
        } else if state.contains("progress") || state.contains("started") {
            Self::InProgress
        } else {
            Self::Todo
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardCardHitArea {
    pub(crate) key: crate::app::state::WorkItemKey,
    pub(crate) column: usize,
    pub(crate) row: usize,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TicketBoardLayout {
    pub(crate) list_toggle: Rect,
    pub(crate) board_toggle: Rect,
    pub(crate) filter_toggle: Rect,
    pub(crate) visible_columns: std::ops::Range<usize>,
    pub(crate) cards: Vec<BoardCardHitArea>,
    pub(crate) column_areas: Vec<Rect>,
    pub(crate) done_column: Option<Rect>,
    pub(crate) spawn_buttons: Vec<(crate::app::state::WorkItemKey, Rect)>,
}

pub(crate) fn ticket_board_page_size(width: u16) -> usize {
    if width < 100 {
        2
    } else {
        5
    }
}

pub(crate) fn ticket_board_items<'a>(
    app: &'a AppState,
    state: &'a WorkViewState,
) -> Vec<TicketItem<'a>> {
    let Some(snapshot) = state.snapshot.as_ref() else {
        return Vec::new();
    };
    let identities = [
        app.work_index_session.linear.viewer.as_deref(),
        app.work_index_session.linear_viewer_identity(),
    ];
    sorted_filtered_tickets(
        &snapshot.items,
        &app.work_item_detail_cache,
        &state.search,
        state.ticket_sort,
        state.ticket_open_only,
        snapshot.observed_at,
        super::dock::pr::focused_pr_key(app).is_some(),
        None,
    )
    .into_iter()
    .filter(|item| {
        !state.board_mine_only
            || identities.iter().flatten().any(|identity| {
                item.summary
                    .assignee
                    .as_deref()
                    .is_some_and(|assignee| assignee.eq_ignore_ascii_case(identity))
            })
    })
    .collect()
}

pub(crate) fn ticket_board_columns(
    app: &AppState,
    state: &WorkViewState,
) -> [Vec<crate::app::state::WorkItemKey>; 5] {
    let mut columns: [Vec<crate::app::state::WorkItemKey>; 5] = std::array::from_fn(|_| Vec::new());
    for item in ticket_board_items(app, state) {
        columns[LinearBoardColumn::for_ticket(item.summary) as usize].push(item.stable_key());
    }
    columns
}

pub(crate) fn ticket_board_layout(
    app: &AppState,
    state: &WorkViewState,
    area: Rect,
) -> TicketBoardLayout {
    let columns = ticket_board_columns(app, state);
    let agents = board_agent_summaries(app);
    ticket_board_layout_from(app, state, area, &columns, &agents)
}

fn ticket_board_layout_from(
    app: &AppState,
    state: &WorkViewState,
    area: Rect,
    columns: &[Vec<crate::app::state::WorkItemKey>; 5],
    agent_summaries: &std::collections::HashMap<String, BoardAgentSummary<'_>>,
) -> TicketBoardLayout {
    if area.width == 0 || area.height < 2 {
        return TicketBoardLayout::default();
    }
    let viewport_width = [
        area.right(),
        app.view.sidebar_rect.right(),
        app.view.dock_rect.right(),
    ]
    .into_iter()
    .max()
    .unwrap_or(area.right());
    let page_size = ticket_board_page_size(viewport_width.max(area.width));
    let page_start = ((state.board_column / page_size) * page_size)
        .min(LinearBoardColumn::ALL.len().saturating_sub(page_size));
    let page_end = (page_start + page_size).min(LinearBoardColumn::ALL.len());
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let constraints = (page_start..page_end)
        .map(|index| {
            if index == LinearBoardColumn::Done as usize && index != state.board_column {
                Constraint::Length(board_done_label(app, columns[4].len()).width() as u16 + 2)
            } else {
                Constraint::Fill(1)
            }
        })
        .collect::<Vec<_>>();
    let column_areas = Layout::horizontal(constraints).split(body);
    let mut cards = Vec::new();
    let mut spawn_buttons = Vec::new();
    let mut done_column = None;
    for (visible_index, column_index) in (page_start..page_end).enumerate() {
        let column_area = column_areas[visible_index];
        let column = LinearBoardColumn::ALL[column_index];
        if column == LinearBoardColumn::Done && column_index != state.board_column {
            done_column = Some(column_area);
            continue;
        }
        let inner = Block::default().borders(Borders::ALL).inner(column_area);
        if inner.width == 0 {
            continue;
        }
        let capacity = usize::from(inner.height) / 4;
        let first = state.board_scroll[column_index]
            .min(columns[column_index].len().saturating_sub(capacity.max(1)));
        for (visible_row, row) in (first..columns[column_index].len())
            .take(capacity)
            .enumerate()
        {
            cards.push(BoardCardHitArea {
                key: columns[column_index][row].clone(),
                column: column_index,
                row,
                rect: Rect::new(inner.x, inner.y + visible_row as u16 * 4, inner.width, 4),
            });
            let card = Rect::new(inner.x, inner.y + visible_row as u16 * 4, inner.width, 4);
            let key = &columns[column_index][row];
            let ticket_id = key.ticket_id.as_deref().unwrap_or_default();
            if card.width >= if app.nerd_font { 3 } else { 2 }
                && !agent_summaries.contains_key(&ticket_id.to_ascii_lowercase())
            {
                spawn_buttons.push((
                    key.clone(),
                    Rect::new(
                        card.right()
                            .saturating_sub(if app.nerd_font { 3 } else { 2 }),
                        card.y + 3,
                        if app.nerd_font { 2 } else { 1 },
                        1,
                    ),
                ));
            }
        }
    }
    let prefix = board_header_prefix(app);
    let list_start = prefix.width() as u16;
    let board_start = list_start + 7;
    let filter_start = board_start + 8;
    let header_hit = |start: u16, width: u16| {
        Rect::new(
            area.x + start.min(area.width),
            area.y,
            width.min(area.width.saturating_sub(start)),
            1,
        )
    };
    TicketBoardLayout {
        list_toggle: header_hit(list_start, 4),
        board_toggle: header_hit(board_start, 5),
        filter_toggle: header_hit(filter_start, board_filter_label(app, state).width() as u16),
        visible_columns: page_start..page_end,
        cards,
        column_areas: column_areas.to_vec(),
        done_column,
        spawn_buttons,
    }
}

fn board_header_prefix(app: &AppState) -> &'static str {
    if app.nerd_font {
        "▦ Board   "
    } else {
        "Board   "
    }
}

fn board_filter_label(app: &AppState, state: &WorkViewState) -> String {
    let scope = if state.board_mine_only { "mine" } else { "all" };
    if app.nerd_font {
        format!("🔍 filter   {scope} ▾")
    } else {
        format!("filter   {scope}")
    }
}

fn board_done_label(app: &AppState, count: usize) -> String {
    if app.nerd_font {
        format!(" ✓ {count} ")
    } else {
        format!(" Done {count} ")
    }
}

fn render_ticket_board(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let refresh = if state.refreshing || !app.work_item_detail_loading.is_empty() {
        " · refreshing…"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            ratatui::text::Span::styled(
                board_header_prefix(app),
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
            ratatui::text::Span::styled("List", Style::default().fg(app.palette.subtext0)),
            ratatui::text::Span::raw(" | "),
            ratatui::text::Span::styled(
                "Board",
                Style::default()
                    .fg(app.palette.accent)
                    .bg(app.palette.surface0)
                    .add_modifier(Modifier::BOLD),
            ),
            ratatui::text::Span::raw("   "),
            ratatui::text::Span::styled(
                board_filter_label(app, state),
                Style::default().fg(app.palette.subtext0),
            ),
            ratatui::text::Span::styled(refresh, Style::default().fg(app.palette.subtext0)),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let columns = ticket_board_columns(app, state);
    let agent_summaries = board_agent_summaries(app);
    let layout = ticket_board_layout_from(app, state, area, &columns, &agent_summaries);
    for (visible_index, column_index) in layout.visible_columns.clone().enumerate() {
        let column = LinearBoardColumn::ALL[column_index];
        let selected_column = column_index == state.board_column;
        let column_area = layout.column_areas[visible_index];
        if column == LinearBoardColumn::Done && !selected_column {
            let strip = Block::default()
                .borders(Borders::ALL)
                .title(board_done_label(app, columns[column_index].len()))
                .border_style(Style::default().fg(app.palette.surface_dim));
            frame.render_widget(strip, column_area);
            continue;
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " {} {} ",
                if column == LinearBoardColumn::Todo {
                    "Todo"
                } else {
                    column.label()
                },
                columns[column_index].len()
            ))
            .border_style(Style::default().fg(if selected_column {
                app.palette.accent
            } else {
                app.palette.surface_dim
            }));
        let inner = block.inner(column_area);
        frame.render_widget(block, column_area);
        if columns[column_index].is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    " —",
                    Style::default().fg(app.palette.overlay0),
                )),
                inner,
            );
            continue;
        }
        for card in layout
            .cards
            .iter()
            .filter(|card| card.column == column_index)
        {
            let Some(ticket_id) = card.key.ticket_id.as_deref() else {
                continue;
            };
            let Some(ticket) = state.snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .items
                    .iter()
                    .flat_map(|item| &item.ticket_details)
                    .find(|ticket| ticket.identifier.eq_ignore_ascii_case(ticket_id))
            }) else {
                continue;
            };
            let selected = selected_column && state.board_rows[column_index] == card.row;
            let style = Style::default().fg(app.palette.text).bg(if selected {
                app.palette.surface0
            } else {
                app.palette.panel_bg
            });
            let width = usize::from(card.rect.width);
            let title = wrap_board_title(
                ticket.title.as_deref().unwrap_or("(untitled)"),
                width.saturating_sub(2),
            );
            let lines = vec![
                Line::styled(board_card_heading(ticket, selected, width), style),
                Line::styled(format!(" {}", title[0]), style),
                Line::styled(format!(" {}", title[1]), style),
                board_agent_line(app, ticket, &agent_summaries, width).style(style),
            ];
            frame.render_widget(Paragraph::new(lines), card.rect);
        }
    }
}

fn board_card_heading(
    ticket: &crate::work_index::WorkTicket,
    selected: bool,
    width: usize,
) -> String {
    if width < 2 {
        return fit_board_text(if selected { "▸" } else { " " }, width);
    }
    let priority = ticket
        .priority
        .map_or("P—".into(), |value| format!("P{value}"));
    let priority = fit_board_text(&priority, width.saturating_sub(2));
    let id_width = width.saturating_sub(priority.width() + 2);
    let id = fit_board_text(&ticket.identifier, id_width);
    let marker = if selected { "▸" } else { " " };
    format!(
        "{marker}{id}{}{priority} ",
        " ".repeat(id_width.saturating_sub(id.width()))
    )
}

fn wrap_board_title(title: &str, width: usize) -> [String; 2] {
    if width == 0 {
        return [String::new(), String::new()];
    }
    let text = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut first = fit_board_text(&text, width);
    if first.len() == text.len() {
        return [first, String::new()];
    }
    if let Some(space) = first
        .rfind(' ')
        .filter(|_| !text[first.len()..].starts_with(' '))
    {
        first.truncate(space);
    }
    let rest = text[first.len()..].trim_start();
    let mut second = fit_board_text(rest, width);
    if second.len() < rest.len() {
        second = fit_board_text(rest, width.saturating_sub(1));
        second.push('…');
    }
    [first, second]
}

fn fit_board_text(value: &str, width: usize) -> String {
    let mut used = 0;
    value
        .chars()
        .take_while(|character| {
            used += unicode_width::UnicodeWidthChar::width(*character).unwrap_or(0);
            used <= width
        })
        .collect()
}

#[derive(Clone, Copy)]
struct BoardAgentSummary<'a> {
    agent: crate::detect::Agent,
    host: &'a str,
    state: crate::detect::AgentState,
    seen: bool,
    activity_at: Option<std::time::Instant>,
}

// One scalar projection per attached pane per board computation. No terminal-core
// locks, snapshots, process probes, or sidebar presentation collection.
fn board_agent_summaries(
    app: &AppState,
) -> std::collections::HashMap<String, BoardAgentSummary<'_>> {
    let mut by_ticket = std::collections::HashMap::new();
    for workspace in &app.workspaces {
        for tab in &workspace.tabs {
            for pane in tab.panes.values() {
                let Some(terminal) = app.terminals.get(&pane.attached_terminal_id) else {
                    continue;
                };
                let Some(agent) = terminal.detected_agent else {
                    continue;
                };
                let context = terminal.effective_work_context();
                if context.ticket_ids.is_empty() {
                    continue;
                }
                let projection = pane.agent_projection(terminal);
                let summary = BoardAgentSummary {
                    agent,
                    host: &app.agent_host_name,
                    state: projection.state,
                    seen: projection.seen,
                    activity_at: terminal.agent_activity_at(),
                };
                for ticket in &context.ticket_ids {
                    by_ticket
                        .entry(ticket.to_ascii_lowercase())
                        .or_insert(summary);
                }
            }
        }
    }
    for remote in &app.remote_agent_panel_entries {
        let Some(agent) = remote.entry.agent else {
            continue;
        };
        if !remote.entry.has_agent {
            continue;
        }
        let summary = BoardAgentSummary {
            agent,
            host: &remote.agent_ref.host,
            state: remote.entry.state,
            seen: remote.entry.seen,
            activity_at: remote.entry.activity_at,
        };
        for ticket in &remote.work_context().ticket_ids {
            by_ticket
                .entry(ticket.to_ascii_lowercase())
                .or_insert(summary);
        }
    }
    by_ticket
}

fn board_agent_state_label(state: crate::detect::AgentState) -> &'static str {
    match state {
        crate::detect::AgentState::Working => "working",
        crate::detect::AgentState::Blocked => "blocked",
        crate::detect::AgentState::Idle => "idle",
        crate::detect::AgentState::Unknown => "unknown",
    }
}

pub(crate) fn ticket_board_next_age_change(
    app: &AppState,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    let state = app.work_view.as_ref()?;
    if state.projection != WorkProjection::Tickets
        || state.ticket_layout != crate::app::state::LinearViewLayout::Board
        || state.board_detail_open
    {
        return None;
    }
    let agents = board_agent_summaries(app);
    let columns = ticket_board_columns(app, state);
    let area = Rect {
        height: app.view.terminal_area.height.saturating_sub(1),
        ..app.view.terminal_area
    };
    let layout = ticket_board_layout_from(app, state, area, &columns, &agents);
    layout
        .cards
        .iter()
        .filter_map(|card| {
            let agent = agents.get(&card.key.ticket_id.as_deref()?.to_ascii_lowercase())?;
            crate::activity_age::next_change_at(agent.activity_at, now)
        })
        .min()
}

fn board_agent_line(
    app: &AppState,
    ticket: &crate::work_index::WorkTicket,
    summaries: &std::collections::HashMap<String, BoardAgentSummary<'_>>,
    width: usize,
) -> Line<'static> {
    let Some(summary) = summaries.get(&ticket.identifier.to_ascii_lowercase()) else {
        let plus = if app.nerd_font { "＋" } else { "+" };
        if width < plus.width() + 1 {
            return Line::styled(
                fit_board_text(" no agent", width),
                Style::default().fg(app.palette.overlay0),
            );
        }
        let label = fit_board_text(" no agent", width.saturating_sub(plus.width() + 1));
        return Line::from(vec![
            ratatui::text::Span::styled(label.clone(), Style::default().fg(app.palette.overlay0)),
            ratatui::text::Span::raw(
                " ".repeat(width.saturating_sub(label.width() + plus.width() + 1)),
            ),
            ratatui::text::Span::raw(if width >= 2 {
                format!("{plus} ")
            } else {
                String::new()
            }),
        ]);
    };
    let agent = crate::ui::icons::agent_label(summary.agent, app.nerd_font).unwrap_or("agent");
    let state = if summary.state == crate::detect::AgentState::Idle && !summary.seen {
        "done"
    } else {
        board_agent_state_label(summary.state)
    };
    let age = crate::activity_age::compact_label(summary.activity_at, app.view_observed_at);
    let dot = crate::ui::sidebar::compact_dot_for_state(
        summary.state,
        summary.seen,
        true,
        false,
        false,
        false,
    );
    let text = fit_board_text(
        &format!("{agent} {} · {state} {age}", summary.host),
        width.saturating_sub(3),
    );
    Line::from(vec![
        ratatui::text::Span::styled(
            fit_board_text(&format!(" {dot} "), width),
            Style::default().fg(super::status::state_label_color(
                summary.state,
                summary.seen,
                &app.palette,
            )),
        ),
        ratatui::text::Span::raw(text),
    ])
}

fn render_review_queue(palette: &Palette, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let scope = state
        .repo_filter
        .as_deref()
        .map(short_repo_name)
        .unwrap_or("all repos");
    let projection = state
        .snapshot
        .as_ref()
        .filter(|_| state.enabled)
        .map(|snapshot| project_review_queue(snapshot, state.repo_filter.as_deref()));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" work · review queue · {scope} "))
        .border_style(Style::default().fg(palette.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(palette.subtext0)),
            inner,
        );
        return;
    }

    let Some(projection) = projection else {
        return;
    };
    let mut lines = Vec::new();
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.short_unavailable_reason(
            crate::work_index::WorkIndexSource::Github,
            std::time::SystemTime::now(),
        )
    }) {
        lines.push(Line::styled(
            format!("GitHub: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    lines.push(Line::styled(
        format_review_queue_summary(
            projection.awaiting_review_count,
            projection.ticket_in_review_count,
            inner.width,
        ),
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD),
    ));
    if projection.rows.is_empty() {
        lines.push(Line::styled(
            "  no PRs awaiting review",
            Style::default().fg(palette.subtext0),
        ));
    } else {
        let selected = state.selected_review_queue_index(&projection).unwrap_or(0);
        for (index, row) in projection.rows.iter().enumerate() {
            let style = if index == selected {
                Style::default()
                    .fg(palette.text)
                    .bg(palette.surface0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text)
            };
            lines.push(Line::styled(
                format_review_queue_row(row, inner.width),
                style,
            ));
        }
    }
    lines.push(Line::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(palette.surface1),
    ));
    lines.push(Line::styled(
        format!(
            "  drift {} PRs awaiting review whose ticket is not In Review",
            projection.drift_count
        ),
        Style::default().fg(palette.subtext0),
    ));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_pull_requests(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let palette = &app.palette;
    let observed_at = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.observed_at)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let items = state.snapshot.as_ref().map(|snapshot| {
        sorted_filtered_prs(
            &snapshot.items,
            &app.work_item_detail_cache,
            &state.search,
            state.sort,
            state.open_only,
            observed_at,
            Some((&app.sidebar_work_filter, &app.work_index_session)),
        )
    });
    let refresh = if state.refreshing || !app.work_item_detail_loading.is_empty() {
        " · refreshing…"
    } else {
        ""
    };
    let columns = detail_columns(area);
    let left = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Pull requests{refresh} "))
        .border_style(Style::default().fg(palette.accent));
    let left_inner = left.inner(columns[0]);
    frame.render_widget(left, columns[0]);
    let filter = if state.open_only { "open" } else { "all" };
    let cursor = if state.search_focused { "▏" } else { "" };
    let mut lines = vec![Line::styled(
        format!(
            " 🔍 {}{cursor}   ⇅ {}   ⚲ {filter}",
            if state.search.is_empty() {
                "search or label:bug"
            } else {
                &state.search
            },
            state.sort.label()
        ),
        Style::default().fg(palette.subtext0),
    )];
    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        lines.push(Line::styled(message, Style::default().fg(palette.subtext0)));
        frame.render_widget(Paragraph::new(lines), left_inner);
        return;
    }
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.short_unavailable_reason(
            crate::work_index::WorkIndexSource::Github,
            std::time::SystemTime::now(),
        )
    }) {
        lines.push(Line::styled(
            format!("GitHub: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    let items = items.unwrap_or_default();
    let selected = state
        .selected
        .as_ref()
        .and_then(|key| {
            let selected_key = format!("{}#{}", key.repo, key.pr_number.unwrap_or_default());
            items.iter().position(|item| item.key() == selected_key)
        })
        .unwrap_or(0);
    let mut current_group = "";
    for (index, item) in items.iter().enumerate() {
        let row = item.row();
        if row.group != current_group {
            current_group = row.group;
            lines.push(Line::styled(
                format!(" {current_group}"),
                Style::default()
                    .fg(palette.subtext0)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        push_work_row(
            &mut lines,
            &row,
            index == selected,
            palette,
            left_inner.width,
            true,
        );
    }
    if items.is_empty() {
        lines.push(Line::styled(
            " no matching pull requests",
            Style::default().fg(palette.subtext0),
        ));
    }
    frame.render_widget(Paragraph::new(lines), left_inner);

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(" Pull request ")
        .border_style(Style::default().fg(palette.accent));
    let detail_inner = detail_block.inner(columns[1]);
    frame.render_widget(detail_block, columns[1]);
    if let Some(item) = items.get(selected) {
        let object_view = state
            .object_views
            .get(&item.stable_key())
            .cloned()
            .unwrap_or_default();
        render_pr_detail(
            app,
            item,
            &object_view,
            PrDetailControls {
                checkout_menu: state.checkout_menu,
                action_menu: state.pr_action_menu,
                reviewer_picker: state.reviewer_picker.as_ref(),
                pending_write: state.pending_write.as_ref(),
                notice: state.hint.as_deref(),
            },
            detail_inner,
            frame,
        );
    } else if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.unavailable_reason(crate::work_index::WorkIndexSource::Github)
    }) {
        frame.render_widget(Paragraph::new(format!(" details\n {reason}")), detail_inner);
    }
}

fn push_work_row(
    lines: &mut Vec<Line<'static>>,
    row: &WorkRow,
    selected: bool,
    palette: &Palette,
    width: u16,
    metadata_on_second_line: bool,
) {
    let style = if selected {
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text)
    };
    let suffix = if metadata_on_second_line {
        row.age.clone()
    } else {
        format!("{}  {}", row.changes, row.age)
    };
    let prefix = if metadata_on_second_line {
        String::new()
    } else {
        format!("{} ", row.metadata)
    };
    let available =
        usize::from(width).saturating_sub(prefix.chars().count() + suffix.chars().count() + 5);
    lines.push(Line::styled(
        format!(
            " {} {}{}  {}",
            row.glyph,
            prefix,
            fit_cell(&row.title, available),
            suffix
        ),
        style,
    ));
    if metadata_on_second_line {
        lines.push(Line::styled(
            format!("   {}  {}", row.metadata, row.changes),
            Style::default().fg(palette.subtext0),
        ));
    }
}

fn push_missive_row(
    lines: &mut Vec<Line<'static>>,
    item: &ConversationItem<'_>,
    row: &WorkRow,
    selected: bool,
    palette: &Palette,
    width: u16,
) {
    let base = if selected {
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text)
    };
    let status = crate::ui::work_status::WorkGroupStatus::from_conversation(
        item.summary.closed,
        !item.summary.assignees.is_empty(),
    );
    let available = usize::from(width).saturating_sub(row.age.chars().count() + 5);
    lines.push(Line::from(vec![
        ratatui::text::Span::styled(" ", base),
        ratatui::text::Span::styled(status.glyph(), base.fg(status.color(palette))),
        ratatui::text::Span::styled(
            format!(" {}  {}", fit_cell(&row.title, available), row.age),
            base,
        ),
    ]));
    lines.push(Line::styled(
        format!("   {}", row.metadata),
        if selected {
            Style::default().fg(palette.subtext0).bg(palette.surface0)
        } else {
            Style::default().fg(palette.subtext0)
        },
    ));
}

#[derive(Clone, Copy, Default)]
pub(crate) struct TicketDetailControls<'a> {
    pub(crate) start_menu: Option<crate::app::state::PrCheckoutChoice>,
    pub(crate) transition_menu: Option<TicketTransitionChoice>,
    pub(crate) action_menu: Option<crate::ui::ticket_actions::TicketActionMenuState>,
    pub(crate) comment_draft: Option<&'a str>,
    pub(crate) pending_write: Option<&'a crate::work_index::WorkItemWrite>,
    pub(crate) notice: Option<&'a str>,
}

/// A detail's rendered lines together with where its foldable section headers
/// landed. The input layer rebuilds this to hit-test and to number the alt
/// digits, so the render stays the only place that decides which sections a
/// view shows and in what order.
pub(crate) struct DetailLayout {
    pub(crate) lines: Vec<Line<'static>>,
    /// One entry per foldable section in render order: the line index of its
    /// header rule, and the section that rule folds.
    pub(crate) sections: Vec<(usize, crate::app::state::DetailSection)>,
    /// Rows the action buttons occupy, for anchoring the menus over them.
    pub(crate) action_rows: usize,
    /// Line the action row starts on, for the same anchoring.
    pub(crate) action_row: usize,
    /// Line the sub-tab row sits on, for anchoring its narrow-width picker.
    pub(crate) tab_row: usize,
    /// Where the reviewer picker opens, when the view has one.
    pub(crate) reviewer_anchor: Option<Rect>,
}

/// Push a foldable section header. Returns whether its body should follow.
fn push_section(
    lines: &mut Vec<Line<'static>>,
    sections: &mut Vec<(usize, crate::app::state::DetailSection)>,
    palette: &Palette,
    title: impl AsRef<str>,
    width: u16,
    section: crate::app::state::DetailSection,
    view: &ObjectViewState,
) -> bool {
    let digit = crate::app::state::DetailSection::toggle_digit(sections.len());
    let collapsed = view.section_is_collapsed(section);
    let [blank, rule] = crate::ui::work_list_detail::collapsible_section_separator(
        palette, title, width, collapsed, digit,
    );
    lines.push(blank);
    sections.push((lines.len(), section));
    lines.push(rule);
    !collapsed
}

pub(crate) fn ticket_detail_layout(
    app: &AppState,
    item: &TicketItem<'_>,
    view: &ObjectViewState,
    controls: &TicketDetailControls<'_>,
    area: Rect,
) -> DetailLayout {
    let palette = &app.palette;
    let detail = item.detail();
    let mut sections = Vec::new();
    let mut lines = vec![Line::from(vec![ratatui::text::Span::styled(
        format!(" {}", detail.heading),
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    )])];
    let action_rows = push_ticket_action_rows(&mut lines, palette, area.width);
    lines.extend([
        Line::styled(
            format!(" {}", detail.title),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.byline),
            Style::default().fg(palette.subtext0),
        ),
    ]);
    if push_section(
        &mut lines,
        &mut sections,
        palette,
        format!("Linked PRs  {}", detail.linked_prs.len()),
        area.width,
        crate::app::state::DetailSection::LinkedPrs,
        view,
    ) {
        for pr in &detail.linked_prs {
            let check = match pr.check_state {
                crate::work_index::PrCheckState::Passing => "✓",
                crate::work_index::PrCheckState::Failing => "✗",
                crate::work_index::PrCheckState::Pending => "◌",
                crate::work_index::PrCheckState::Unknown => "—",
            };
            lines.push(Line::styled(
                format!("  ⑂ #{} {}  {check}", pr.number, pr.title),
                Style::default().fg(palette.subtext0),
            ));
        }
    }
    if push_section(
        &mut lines,
        &mut sections,
        palette,
        "Description",
        area.width,
        crate::app::state::DetailSection::Description,
        view,
    ) {
        lines.extend(crate::ui::markdown::body_lines(
            palette,
            crate::ui::work_list_detail::description_without_checklist(
                detail.description.as_deref(),
            )
            .as_deref(),
            usize::from(area.width.saturating_sub(2)),
            " ",
        ));
    }
    if !detail.checks.is_empty()
        && push_section(
            &mut lines,
            &mut sections,
            palette,
            "Acceptance criteria",
            area.width,
            crate::app::state::DetailSection::AcceptanceCriteria,
            view,
        )
    {
        for (text, state) in &detail.checks {
            lines.push(Line::styled(
                format!("  {} {text}", if state == "done" { "✓" } else { "✗" }),
                Style::default().fg(palette.subtext0),
            ));
        }
    }
    if push_section(
        &mut lines,
        &mut sections,
        palette,
        format!("Comments  {}  newest first", detail.comments.len()),
        area.width,
        crate::app::state::DetailSection::Comments,
        view,
    ) {
        push_comment_lines(
            &mut lines,
            palette,
            &detail.comments,
            item.observed_at,
            view,
            area.width,
            "  ",
            None,
        );
    }
    if let Some(draft) = controls.comment_draft {
        lines.push(Line::styled(
            format!(" Comment: {draft}▏  Enter to stage · Esc cancel"),
            Style::default().fg(palette.yellow),
        ));
    }
    if let Some(write) = controls.pending_write {
        lines.push(Line::styled(
            format!(" Confirm {}? [y/N]", write.describe()),
            Style::default()
                .fg(palette.yellow)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(notice) = controls.notice {
        lines.push(Line::styled(
            format!(" {notice}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    DetailLayout {
        lines,
        sections,
        action_rows,
        action_row: 0,
        tab_row: 0,
        reviewer_anchor: None,
    }
}

/// One comment block per comment: a dim header then its body, cut to a
/// readable head unless the reader expanded it. `decoration` is the glyph and
/// trailing tag the pull-request host puts around its headers.
// Eight arguments because both hosts render comments from the same loop and
// each one is a distinct render input: the two hosts differ only in indent and
// decoration, so bundling them into a struct would name a type used once.
#[allow(clippy::too_many_arguments)]
fn push_comment_lines(
    lines: &mut Vec<Line<'static>>,
    palette: &Palette,
    comments: &[crate::work_index::WorkItemComment],
    observed_at: std::time::SystemTime,
    view: &ObjectViewState,
    width: u16,
    indent: &str,
    decoration: Option<(&str, &str)>,
) {
    for (index, comment) in comments.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        let header = match decoration {
            Some((badge, tag)) => format!(
                "{indent}{badge} {}  {tag}",
                comment_header(comment, observed_at)
            ),
            None => format!("{indent}{}", comment_header(comment, observed_at)),
        };
        lines.push(Line::styled(
            header,
            Style::default()
                .fg(palette.subtext0)
                .add_modifier(Modifier::DIM),
        ));
        lines.extend(comment_body_lines(
            palette,
            comment,
            index,
            usize::from(width.saturating_sub(4)),
            "    ",
            view.comment_is_expanded(comment_identity(comment)),
        ));
    }
}

/// One line per check. The overview and the checks sub-tab differ only in what
/// they say when there are none, which the overview leaves out entirely.
fn push_check_lines(
    lines: &mut Vec<Line<'static>>,
    palette: &Palette,
    checks: &[(String, String)],
    say_when_empty: bool,
) {
    for (name, status) in checks {
        let glyph = match status.as_str() {
            "SUCCESS" => "✓",
            "FAILURE" => "✗",
            _ => "◌",
        };
        lines.push(Line::styled(
            format!("  {glyph} {name}  {status}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    if say_when_empty && checks.is_empty() {
        lines.push(Line::styled(
            " no checks reported",
            Style::default().fg(palette.subtext0),
        ));
    }
}

pub(crate) fn render_ticket_detail(
    app: &AppState,
    item: &TicketItem<'_>,
    view: &ObjectViewState,
    controls: TicketDetailControls<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    let palette = &app.palette;
    let layout = ticket_detail_layout(app, item, view, &controls, area);
    let action_rows = layout.action_rows;
    let max_scroll = layout.lines.len().saturating_sub(usize::from(area.height));
    let scroll = usize::from(view.scroll).min(max_scroll) as u16;
    frame.render_widget(Paragraph::new(layout.lines).scroll((scroll, 0)), area);

    if let Some(choice) = controls.start_menu {
        render_ticket_start_menu(app, frame, area, item, choice);
    } else if let Some(choice) = controls.transition_menu {
        render_ticket_transition_menu(app, frame, area, item, choice);
    } else if let Some(menu) = controls.action_menu {
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            item.summary,
            item.cached_detail,
            app.work_index_session.linear.viewer.as_deref(),
            app.work_index_session.linear_viewer_identity(),
            item.has_context_pr,
        );
        crate::ui::ticket_actions::render_ticket_action_menu(
            palette,
            frame,
            area,
            ticket_action_menu_anchor(area, action_rows),
            &context,
            menu,
        );
    }
}

/// The two ticket controls the detail draws as buttons. The render and the
/// input layer share these labels so a click lands on exactly what was drawn.
/// The rows are spelled out rather than formatted because they are pushed on
/// every ticket-detail render.
pub(crate) const TICKET_START_LABEL: &str = "[Start thread ▾]";
pub(crate) const TICKET_MORE_LABEL: &str = "[⋯]";
const TICKET_ACTION_ROW: &str = " [Start thread ▾] [⋯]";
const TICKET_START_ROW: &str = " [Start thread ▾]";
const TICKET_MORE_ROW: &str = " [⋯]";

/// Which ticket control the pointer is over, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TicketActionControl {
    StartThread,
    More,
}

fn push_ticket_action_rows(lines: &mut Vec<Line<'static>>, palette: &Palette, width: u16) -> usize {
    let style = Style::default().fg(palette.accent);
    let action_width = crate::ui::text::display_width(TICKET_ACTION_ROW);
    if width >= 60 && action_width <= usize::from(width) {
        lines.push(Line::styled(TICKET_ACTION_ROW, style));
        1
    } else {
        lines.push(Line::styled(TICKET_START_ROW, style));
        lines.push(Line::styled(TICKET_MORE_ROW, style));
        2
    }
}

/// The control under the pointer, given the same geometry the render used.
///
/// The action rows follow the heading, so their line indices are 1 and, when
/// the detail is too narrow for one row, 2. `scroll` is the detail's own
/// scroll, which moves the buttons with the rest of the body.
pub(crate) fn ticket_action_control_at(
    area: Rect,
    action_rows: usize,
    scroll: u16,
    column: u16,
    row: u16,
) -> Option<TicketActionControl> {
    if area.width == 0 || area.height == 0 || column < area.x || column >= area.right() {
        return None;
    }
    let screen_row = |line: u16| -> Option<u16> {
        let offset = line.checked_sub(scroll)?;
        let row = area.y.checked_add(offset)?;
        (row < area.bottom()).then_some(row)
    };
    let start_width = crate::ui::text::display_width(TICKET_START_LABEL) as u16;
    let more_width = crate::ui::text::display_width(TICKET_MORE_LABEL) as u16;
    let hit = |x: u16, width: u16| column >= x && column < x.saturating_add(width);
    let start_x = area.x.saturating_add(1);
    if action_rows <= 1 {
        if screen_row(1) != Some(row) {
            return None;
        }
        if hit(start_x, start_width) {
            return Some(TicketActionControl::StartThread);
        }
        let more_x = start_x.saturating_add(start_width).saturating_add(1);
        return hit(more_x, more_width).then_some(TicketActionControl::More);
    }
    if screen_row(1) == Some(row) && hit(start_x, start_width) {
        return Some(TicketActionControl::StartThread);
    }
    if screen_row(2) == Some(row) && hit(start_x, more_width) {
        return Some(TicketActionControl::More);
    }
    None
}

/// Rows of the start-thread dropdown, for hit-testing the same rect the render
/// draws. Kept beside the render so both read one anchor.
pub(crate) fn ticket_start_menu_layout(area: Rect) -> Option<crate::ui::dropdown::DropdownLayout> {
    ticket_menu_layout(area, 12, 1, 2, 0, 42)
}

pub(crate) fn ticket_action_menu_anchor(area: Rect, action_rows: usize) -> Rect {
    Rect::new(
        area.right().saturating_sub(3),
        area.y.saturating_add(action_rows as u16),
        3.min(area.width),
        1,
    )
}

#[derive(Clone, Copy, Default)]
pub(crate) struct PrDetailControls<'a> {
    pub(crate) checkout_menu: Option<crate::app::state::PrCheckoutChoice>,
    pub(crate) action_menu: Option<crate::app::state::PrActionMenuState>,
    pub(crate) reviewer_picker: Option<&'a crate::app::state::ReviewerPickerState>,
    pub(crate) pending_write: Option<&'a crate::work_index::WorkItemWrite>,
    pub(crate) notice: Option<&'a str>,
}

/// Render one pull request. The full-screen work view and dock call this exact
/// function with the same object state, so width is the only host difference.
pub(crate) fn pr_detail_layout(
    app: &AppState,
    item: &crate::ui::work_list_detail::PrItem<'_>,
    view: &ObjectViewState,
    controls: &PrDetailControls<'_>,
    area: Rect,
) -> DetailLayout {
    let palette = &app.palette;
    let detail = item.detail();
    let checkout_available = detail.open_url.is_some()
        && item
            .cached_detail
            .and_then(|detail| detail.head_ref_name.as_ref())
            .is_some();
    let actions = item.action_table(app.pr_merge_method, checkout_available);
    let merge = actions.iter().find(|action| {
        matches!(
            action.placement,
            crate::ui::work_list_detail::PrActionPlacement::Header
        ) && matches!(
            action.kind,
            crate::ui::work_list_detail::PrActionKind::Merge(_)
        )
    });
    let merge_style = if merge.is_some_and(|action| action.enabled()) {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::DIM)
    };
    let checkout = actions.iter().find(|action| {
        action.kind == crate::ui::work_list_detail::PrActionKind::CheckOut
            && matches!(
                action.placement,
                crate::ui::work_list_detail::PrActionPlacement::Header
            )
    });
    let mut lines = vec![
        Line::styled(
            format!(" {}", detail.heading),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.title),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.byline),
            Style::default().fg(palette.subtext0),
        ),
        Line::styled(
            format!(" {}", detail.branches),
            Style::default().fg(palette.subtext0),
        ),
    ];
    let mut sections = Vec::new();
    let mut action_row = lines.len();
    let action_rows = push_pr_action_rows(
        &mut lines,
        palette,
        checkout.map_or("Check out ▾", |action| action.label.as_str()),
        merge.map_or("Merge", |action| action.label.as_str()),
        merge_style,
        area.width,
    );
    let mut tab_row = lines.len();
    lines.push(pr_tab_row(
        palette,
        view.tab,
        area.width,
        &detail.checks_summary,
    ));
    if let Some(reason) = merge.and_then(|action| action.disabled_reason) {
        lines.insert(
            1,
            Line::styled(
                format!(" {reason}"),
                Style::default()
                    .fg(palette.overlay0)
                    .add_modifier(Modifier::DIM),
            ),
        );
        action_row = action_row.saturating_add(1);
        tab_row = tab_row.saturating_add(1);
    }
    let mut reviewer_anchor = None;
    match view.tab {
        PrDetailTab::Overview => {
            reviewer_anchor = Some(Rect::new(
                area.right().saturating_sub(4),
                area.y
                    .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX)),
                4.min(area.width),
                1,
            ));
            lines.push(Line::styled(
                format!(" Reviewers  {}  ✦ +", detail.reviewers),
                Style::default().fg(palette.text),
            ));
            if push_section(
                &mut lines,
                &mut sections,
                palette,
                "Description",
                area.width,
                crate::app::state::DetailSection::Description,
                view,
            ) {
                lines.extend(crate::ui::markdown::body_lines(
                    palette,
                    detail.description.as_deref(),
                    usize::from(area.width.saturating_sub(2)),
                    " ",
                ));
            }
            if push_section(
                &mut lines,
                &mut sections,
                palette,
                format!("Checks  {}", detail.checks.len()),
                area.width,
                crate::app::state::DetailSection::Checks,
                view,
            ) {
                push_check_lines(&mut lines, palette, &detail.checks, false);
            }
            if push_section(
                &mut lines,
                &mut sections,
                palette,
                format!("Comments  {}  newest first", detail.comments.len()),
                area.width,
                crate::app::state::DetailSection::Comments,
                view,
            ) {
                push_comment_lines(
                    &mut lines,
                    palette,
                    &detail.comments,
                    item.observed_at,
                    view,
                    area.width,
                    "  ",
                    Some(("✦", "[Fix in a thread]")),
                );
            }
        }
        PrDetailTab::Files => {
            let files = item
                .cached_detail
                .map(|detail| detail.files.as_slice())
                .unwrap_or_default();
            if push_section(
                &mut lines,
                &mut sections,
                palette,
                format!("Files  {}", files.len()),
                area.width,
                crate::app::state::DetailSection::Files,
                view,
            ) {
                for file in files {
                    lines.push(Line::styled(
                        format!("  {}  +{} −{}", file.path, file.additions, file.deletions),
                        Style::default().fg(palette.text),
                    ));
                }
                if files.is_empty() {
                    lines.push(Line::styled(
                        " no changed files indexed",
                        Style::default().fg(palette.subtext0),
                    ));
                }
            }
        }
        PrDetailTab::Diff => {}
        PrDetailTab::Checks => {
            if push_section(
                &mut lines,
                &mut sections,
                palette,
                format!("Checks  {}", detail.checks.len()),
                area.width,
                crate::app::state::DetailSection::Checks,
                view,
            ) {
                push_check_lines(&mut lines, palette, &detail.checks, true);
            }
        }
        PrDetailTab::Timeline => {
            if !push_section(
                &mut lines,
                &mut sections,
                palette,
                "Timeline · newest first",
                area.width,
                crate::app::state::DetailSection::Timeline,
                view,
            ) {
                // Collapsed: the header is the whole section.
            } else {
                if let Some(message) = item
                    .cached_detail
                    .and_then(|detail| detail.timeline_unavailable.as_deref())
                {
                    lines.push(Line::styled(
                        format!(" {message}"),
                        Style::default().fg(palette.red),
                    ));
                }
                let timeline = item
                    .cached_detail
                    .map(|detail| detail.timeline.as_slice())
                    .unwrap_or_default();
                for (index, event) in timeline.iter().enumerate() {
                    if index > 0 {
                        lines.push(Line::default());
                    }
                    let age = event
                        .created_at
                        .map(|created_at| {
                            comment_header(
                                &crate::work_index::WorkItemComment {
                                    author: event.actor.clone(),
                                    body: String::new(),
                                    created_at: Some(created_at),
                                },
                                item.observed_at,
                            )
                        })
                        .unwrap_or_else(|| {
                            format!("{} · —", event.actor.as_deref().unwrap_or("unknown"))
                        });
                    lines.push(Line::styled(
                        format!("  {} · {}", event.kind.replace('_', " "), age),
                        Style::default()
                            .fg(palette.subtext0)
                            .add_modifier(Modifier::DIM),
                    ));
                    lines.extend(crate::ui::markdown::body_lines(
                        palette,
                        Some(&event.summary),
                        usize::from(area.width.saturating_sub(4)),
                        "    ",
                    ));
                }
                if timeline.is_empty()
                    && item
                        .cached_detail
                        .is_some_and(|detail| detail.timeline_unavailable.is_none())
                {
                    lines.push(Line::styled(
                        " no timeline events",
                        Style::default().fg(palette.subtext0),
                    ));
                }
            }
        }
    }
    if let Some(write) = controls.pending_write {
        lines.push(Line::styled(
            format!(" Confirm {}? [y/N]", write.describe()),
            Style::default()
                .fg(palette.yellow)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(notice) = controls.notice {
        lines.push(Line::styled(
            format!(" {notice}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    DetailLayout {
        lines,
        sections,
        action_rows,
        action_row,
        tab_row,
        reviewer_anchor,
    }
}

pub(crate) fn render_pr_detail(
    app: &AppState,
    item: &crate::ui::work_list_detail::PrItem<'_>,
    view: &ObjectViewState,
    controls: PrDetailControls<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    let palette = &app.palette;
    let detail = item.detail();
    let checkout_available = detail.open_url.is_some()
        && item
            .cached_detail
            .and_then(|detail| detail.head_ref_name.as_ref())
            .is_some();
    let actions = item.action_table(app.pr_merge_method, checkout_available);
    let DetailLayout {
        lines,
        sections: _,
        action_rows,
        action_row,
        tab_row,
        reviewer_anchor,
    } = pr_detail_layout(app, item, view, &controls, area);
    if view.tab == PrDetailTab::Diff {
        let header_height = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .min(area.height);
        let [header, body] =
            Layout::vertical([Constraint::Length(header_height), Constraint::Min(0)]).areas(area);
        frame.render_widget(Paragraph::new(lines), header);
        crate::ui::dock::diff::render_diff_with_state(app, frame, body, view.scroll, false);
    } else {
        let max_scroll = lines.len().saturating_sub(usize::from(area.height));
        let scroll = usize::from(view.scroll).min(max_scroll) as u16;
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
    }
    if let (Some(picker), Some(anchor), Some(detail)) = (
        controls.reviewer_picker,
        reviewer_anchor,
        item.cached_detail,
    ) {
        render_reviewer_picker(app, frame, area, anchor, picker, &detail.collaborators);
    }
    if let Some(choice) = controls.checkout_menu {
        if let Some(layout) = checkout_menu_layout(area, action_row, choice) {
            let options = [
                crate::app::state::PrCheckoutChoice::CurrentCheckout,
                crate::app::state::PrCheckoutChoice::NewWorktree,
            ];
            let menu = options
                .iter()
                .map(|option| {
                    Line::styled(
                        format!(
                            "{} {}",
                            if *option == choice { "▸" } else { " " },
                            match option {
                                crate::app::state::PrCheckoutChoice::CurrentCheckout =>
                                    "Current checkout",
                                crate::app::state::PrCheckoutChoice::NewWorktree => "New worktree",
                            }
                        ),
                        Style::default().fg(palette.text).bg(palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(menu), layout.rect);
        } else {
            frame.render_widget(
                Paragraph::new("checkout menu needs space below")
                    .style(Style::default().fg(palette.red)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
    } else if let Some(menu) = controls.action_menu {
        let anchor = pr_action_menu_anchor(
            area,
            action_row.saturating_add(action_rows.saturating_sub(1)),
        );
        if !crate::ui::pr_actions::render(app, frame, area, anchor, &actions, menu) {
            frame.render_widget(
                Paragraph::new("action menu needs space below")
                    .style(Style::default().fg(palette.red)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
    }
    if let Some(selected) = view.tab_picker {
        render_pr_tab_picker(app, frame, area, tab_row, selected);
    }
}

fn push_pr_action_rows(
    lines: &mut Vec<Line<'static>>,
    palette: &Palette,
    checkout: &str,
    merge: &str,
    merge_style: Style,
    width: u16,
) -> usize {
    let checkout_text = format!(" [{checkout}]");
    let merge_text = format!(" [{merge}]");
    let menu_text = " [⋯]";
    let total = crate::ui::text::display_width(&checkout_text)
        + crate::ui::text::display_width(&merge_text)
        + crate::ui::text::display_width(menu_text);
    let checkout_style = Style::default().fg(palette.accent);
    if width >= 60 && total <= usize::from(width) {
        lines.push(Line::from(vec![
            ratatui::text::Span::styled(checkout_text, checkout_style),
            ratatui::text::Span::styled(merge_text, merge_style),
            ratatui::text::Span::styled(menu_text, Style::default().fg(palette.accent)),
        ]));
        1
    } else {
        lines.push(Line::styled(checkout_text, checkout_style));
        lines.push(Line::from(vec![
            ratatui::text::Span::styled(merge_text, merge_style),
            ratatui::text::Span::styled(menu_text, Style::default().fg(palette.accent)),
        ]));
        2
    }
}

fn pr_tab_row(
    palette: &Palette,
    selected: PrDetailTab,
    width: u16,
    checks_summary: &str,
) -> Line<'static> {
    if width < 60 {
        return Line::styled(
            format!(" [{} ▾]  {checks_summary}", selected.label()),
            Style::default().fg(palette.accent),
        );
    }
    let mut spans = vec![ratatui::text::Span::raw(" ")];
    for tab in PrDetailTab::ALL {
        let style = Style::default()
            .fg(if tab == selected {
                palette.text
            } else {
                palette.subtext0
            })
            .add_modifier(if tab == selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        spans.push(ratatui::text::Span::styled(
            format!("[{}] ", tab.label()),
            style,
        ));
    }
    Line::from(spans)
}

fn render_pr_tab_picker(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    tab_row: usize,
    selected: usize,
) {
    let Some(layout) = pr_tab_picker_layout(area, tab_row, selected) else {
        return;
    };
    let rows = PrDetailTab::ALL
        .iter()
        .map(|tab| crate::ui::dropdown::DropdownMenuRow::Item {
            label: tab.label().to_string(),
            enabled: true,
        })
        .collect::<Vec<_>>();
    crate::ui::dropdown::render_menu(&app.palette, frame, &layout, &rows, selected);
}

fn pr_tab_picker_layout(
    area: Rect,
    tab_row: usize,
    selected: usize,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    let anchor_row = area.y.saturating_add(
        u16::try_from(tab_row)
            .unwrap_or(u16::MAX)
            .min(area.height.saturating_sub(1)),
    );
    let anchor = Rect::new(area.x.saturating_add(1), anchor_row, 16.min(area.width), 1);
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor,
            item_count: PrDetailTab::ALL.len(),
            selected,
            has_filter: false,
            max_rows: PrDetailTab::ALL.len(),
            min_width: 18,
        },
        area,
    )
}

fn render_reviewer_picker(
    app: &AppState,
    frame: &mut Frame,
    containing_area: Rect,
    anchor: Rect,
    picker: &crate::app::state::ReviewerPickerState,
    collaborators: &[String],
) {
    let matches = picker.filter.matches(collaborators);
    let Some(layout) = reviewer_picker_layout(
        containing_area,
        anchor,
        matches.len(),
        picker.filter.selected,
    ) else {
        return;
    };
    let rows = matches
        .iter()
        .map(|(_, login)| crate::ui::dropdown::DropdownMenuRow::Item {
            label: (*login).to_string(),
            enabled: true,
        })
        .collect::<Vec<_>>();
    crate::ui::dropdown::render_menu(&app.palette, frame, &layout, &rows, picker.filter.selected);
    if let Some(filter_rect) = layout.filter_rect {
        frame.render_widget(
            Paragraph::new(format!(" 🔍 {}▏", picker.filter.query)).style(
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.panel_bg),
            ),
            filter_rect,
        );
    }
}

fn reviewer_picker_layout(
    containing_area: Rect,
    anchor: Rect,
    item_count: usize,
    selected: usize,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor,
            item_count,
            selected,
            has_filter: true,
            max_rows: 8,
            min_width: 28,
        },
        containing_area,
    )
}

fn pr_action_menu_anchor(area: Rect, action_row: usize) -> Rect {
    Rect::new(
        area.right().saturating_sub(3),
        area.y.saturating_add(
            u16::try_from(action_row)
                .unwrap_or(u16::MAX)
                .min(area.height.saturating_sub(1)),
        ),
        3.min(area.width),
        1,
    )
}

fn checkout_menu_layout(
    area: Rect,
    action_row: usize,
    selected: crate::app::state::PrCheckoutChoice,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    let anchor = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(
            u16::try_from(action_row)
                .unwrap_or(u16::MAX)
                .min(area.height.saturating_sub(1)),
        ),
        14.min(area.width),
        1,
    );
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor,
            item_count: 2,
            selected: usize::from(selected == crate::app::state::PrCheckoutChoice::NewWorktree),
            has_filter: false,
            max_rows: 2,
            min_width: 18,
        },
        area,
    )
}

fn ticket_menu_layout(
    area: Rect,
    anchor_x: u16,
    anchor_y: u16,
    item_count: usize,
    selected: usize,
    width: u16,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor: Rect::new(
                area.x.saturating_add(anchor_x).min(area.right()),
                area.y.saturating_add(anchor_y),
                width.min(area.width),
                1,
            ),
            item_count,
            selected,
            has_filter: false,
            max_rows: item_count,
            min_width: width,
        },
        area,
    )
}

fn render_ticket_start_menu(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    item: &TicketItem<'_>,
    choice: crate::app::state::PrCheckoutChoice,
) {
    let Some(layout) = ticket_start_menu_layout(area) else {
        return;
    };
    let branch = crate::ui::work_list_detail::ticket_worktree_branch(
        &app.branch_prefix,
        &item.summary.identifier,
        item.summary.title.as_deref().unwrap_or_default(),
    );
    let options = [
        (
            crate::app::state::PrCheckoutChoice::CurrentCheckout,
            "Current checkout".to_string(),
        ),
        (
            crate::app::state::PrCheckoutChoice::NewWorktree,
            format!("New worktree {branch}"),
        ),
    ];
    frame.render_widget(
        Paragraph::new(
            options
                .into_iter()
                .map(|(option, label)| {
                    Line::styled(
                        format!("{} {label}", if option == choice { "▸" } else { " " }),
                        Style::default()
                            .fg(app.palette.text)
                            .bg(app.palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        layout.rect,
    );
}

fn render_ticket_transition_menu(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    item: &TicketItem<'_>,
    choice: TicketTransitionChoice,
) {
    let selected = TicketTransitionChoice::ALL
        .iter()
        .position(|option| *option == choice)
        .unwrap_or(0);
    let Some(layout) = ticket_menu_layout(area, 29, 1, 4, selected, 20) else {
        return;
    };
    frame.render_widget(
        Paragraph::new(
            TicketTransitionChoice::ALL
                .into_iter()
                .map(|option| {
                    let enabled = item.transition_enabled(option.label());
                    Line::styled(
                        format!(
                            "{} {}{}",
                            if option == choice { "▸" } else { " " },
                            option.label(),
                            if enabled { "" } else { " · current" }
                        ),
                        Style::default()
                            .fg(if enabled {
                                app.palette.text
                            } else {
                                app.palette.overlay0
                            })
                            .bg(app.palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        layout.rect,
    );
}

fn render_placeholder(
    palette: &Palette,
    projection: WorkProjection,
    area: Rect,
    frame: &mut Frame,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" work · {} ", projection.label()))
        .border_style(Style::default().fg(palette.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(format!("{} not yet available", projection.label()))
            .style(Style::default().fg(palette.subtext0)),
        inner,
    );
}

fn render_footer(palette: &Palette, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let base = match state.projection {
        WorkProjection::PullRequests => {
            " / search   ↑/↓ move   s sort   f open/all   Tab detail tabs   PgUp/PgDn detail   + reviewer   c checkout   l merge   m actions"
        }
        WorkProjection::Tickets
            if state.ticket_layout == crate::app::state::LinearViewLayout::Board
                && !state.board_detail_open =>
        {
            " ←/→ columns   ↑/↓ cards   Enter detail   m mine/all   t transition   v list"
        }
        WorkProjection::Tickets => {
            " / search   ↑/↓ move   s sort   f open/all   c start   t transition   l link PR   m more   v board"
        }
        WorkProjection::Missive => {
            let wide = " / search   ↑/↓ move   PgUp/PgDn detail   f open/all   c start thread   o Open in Missive   r refresh";
            if crate::ui::text::display_width(wide) <= usize::from(area.width) {
                wide
            } else {
                " / search  ↑/↓ conversation  PgUp/PgDn detail  c start  o copy  r refresh"
            }
        }
        WorkProjection::Agents => " ←/→ view PRs tickets Missive [agents]   not yet available",
        WorkProjection::ReviewQueue => {
            " ←/→ view PRs tickets Missive agents [review queue]   ↑/↓ move   f filter repo"
        }
    };
    let text = state
        .hint
        .as_deref()
        .map_or_else(|| base.to_string(), |hint| format!(" {hint}"));
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(palette.subtext0)),
        area,
    );
}

fn format_review_queue_summary(awaiting: usize, in_review: usize, width: u16) -> String {
    let left = format!("  awaiting review · {awaiting}");
    let right = format!("ticket says \"In Review\" · {in_review}");
    let width = usize::from(width);
    let left_width = width.div_ceil(2);
    format!(
        "{}{}",
        fit_cell(&left, left_width),
        fit_cell(&right, width.saturating_sub(left_width))
    )
}

fn format_review_queue_row(row: &WorkReviewQueueRow, width: u16) -> String {
    let ticket_width = 11;
    let state_width = 13;
    let verdict_width = 13;
    let fixed_width = 2 + 6 + 2 + 2 + ticket_width + 2 + state_width + 2 + verdict_width;
    let title_width = usize::from(width).saturating_sub(fixed_width).max(8);
    format!(
        "  {:<6}  {}  {}  {}  {}",
        row.number,
        fit_cell(&row.title, title_width),
        fit_cell(&row.ticket, ticket_width),
        fit_cell(&row.ticket_state, state_width),
        fit_cell(row.verdict.label(), verdict_width),
    )
}

fn fit_cell(value: &str, width: usize) -> String {
    let mut chars = value.chars();
    let mut cell = chars.by_ref().take(width).collect::<String>();
    if chars.next().is_some() && width > 0 {
        cell.pop();
        cell.push('…');
    }
    let len = cell.chars().count();
    cell.extend(std::iter::repeat_n(' ', width.saturating_sub(len)));
    cell
}

fn short_repo_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::AppState,
        work_index::{Snapshot, WorkItem, WorkItemSource},
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::SystemTime;

    fn pr(repo: &str, number: u64, tickets: &[&str]) -> WorkItem {
        WorkItem {
            repo: repo.to_string(),
            pr_number: Some(number),
            pr_url: Some(format!("https://github.com/{repo}/pull/{number}")),
            pr_title: Some(format!("PR {number}")),
            pr_state: Some("open".to_string()),
            draft: false,
            review_decision: Some("REVIEW_REQUIRED".to_string()),
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Unknown,
            audience: crate::work_index::PrAudience::Other,
            cached_pr_detail: None,
            ticket_ids: tickets.iter().map(|ticket| (*ticket).to_string()).collect(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    fn review_pr(
        repo: &str,
        number: u64,
        title: &str,
        tickets: &[&str],
        ticket_state: Option<&str>,
    ) -> WorkItem {
        let mut item = pr(repo, number, tickets);
        item.pr_title = Some(title.to_string());
        item.ticket_state = ticket_state.map(str::to_string);
        item
    }

    fn snapshot(items: Vec<WorkItem>) -> Snapshot {
        Snapshot {
            items,
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn missive_conversation(message_count: usize) -> crate::work_index::MissiveConversation {
        crate::work_index::MissiveConversation {
            id: "sample".into(),
            subject: "Billing question that stays readable".into(),
            app_url: "missive://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            team: None,
            assignees: vec![crate::work_index::MissiveUser {
                id: "ada".into(),
                name: "Ada Lovelace".into(),
                email: None,
                is_me: true,
            }],
            last_activity_at: Some(SystemTime::UNIX_EPOCH),
            closed: false,
            labels: Vec::new(),
            pane_bound: false,
            messages: (0..message_count)
                .map(|index| crate::work_index::MissiveEntry {
                    id: format!("message-{index}"),
                    author: Some("Customer".into()),
                    preview: format!("message body {index}"),
                    created_at: Some(SystemTime::UNIX_EPOCH),
                })
                .collect(),
            notes: Vec::new(),
            drafts: Vec::new(),
            posts: Vec::new(),
        }
    }

    fn missive_state(message_count: usize) -> WorkViewState {
        let mut state = WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                conversations: vec![missive_conversation(message_count)],
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        );
        state.projection = WorkProjection::Missive;
        state
    }

    fn ticket(identifier: &str, group: crate::work_index::TicketGroup) -> WorkItem {
        WorkItem {
            repo: "owner/repo".into(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Unknown,
            audience: crate::work_index::PrAudience::Unclassified,
            cached_pr_detail: None,
            ticket_ids: vec![identifier.into()],
            ticket_title: Some("ticket title".into()),
            ticket_state: Some("In Progress".into()),
            ticket_details: vec![crate::work_index::WorkTicket {
                identifier: identifier.into(),
                title: Some("ticket title".into()),
                description: Some(
                    "Ticket body with https://example.invalid/a/very/long/path/that/must/wrap.\n- [x] done\n- [ ] left"
                        .into(),
                ),
                state: Some("In Progress".into()),
                assignee: Some("matthias".into()),
                creator: None,
                priority: Some(2),
                cycle: Some("cycle 34".into()),
                group,
                created_at: None,
                updated_at: None,
                branch: None,
                labels: vec!["bug".into()],
                url: Some(format!("https://linear.app/acme/issue/{identifier}")),
                parent: None,
                relations: Vec::new(),
            }],
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    fn board_state() -> WorkViewState {
        let cases = [
            ("SCA-1", "Triage", crate::work_index::TicketGroup::Triage),
            ("SCA-2", "Todo", crate::work_index::TicketGroup::Assigned),
            (
                "SCA-3",
                "In Progress",
                crate::work_index::TicketGroup::Assigned,
            ),
            (
                "SCA-4",
                "In Review",
                crate::work_index::TicketGroup::Assigned,
            ),
            (
                "SCA-5",
                "Done",
                crate::work_index::TicketGroup::DoneThisCycle,
            ),
        ];
        let items = cases
            .into_iter()
            .map(|(identifier, state, group)| {
                let mut item = ticket(identifier, group);
                item.ticket_state = Some(state.into());
                item.ticket_details[0].state = Some(state.into());
                item.ticket_details[0].title = Some(format!("{state} ticket with a wrapped title"));
                item
            })
            .collect();
        let mut state = WorkViewState::new(true, Some(snapshot(items)));
        state.projection = WorkProjection::Tickets;
        state.ticket_layout = crate::app::state::LinearViewLayout::Board;
        state.board_mine_only = false;
        state
    }

    #[test]
    fn board_maps_injected_workflow_states_to_team_columns() {
        let app = AppState::test_new();
        let state = board_state();
        let columns = ticket_board_columns(&app, &state);
        let identifiers = columns.map(|column| {
            column
                .into_iter()
                .filter_map(|key| key.ticket_id)
                .collect::<Vec<_>>()
        });
        assert_eq!(
            identifiers,
            [
                vec![String::from("SCA-1")],
                vec![String::from("SCA-2")],
                vec![String::from("SCA-3")],
                vec![String::from("SCA-4")],
                vec![String::from("SCA-5")],
            ]
        );
    }

    #[test]
    fn board_mine_filter_defaults_on_and_keeps_done_outside_sidebar_status_filters() {
        let mut app = AppState::test_new();
        assert!(WorkViewState::new(true, None).board_mine_only);
        let mut state = board_state();
        state.board_mine_only = true;
        app.work_index_session.linear.viewer = Some("matthias".into());
        state.snapshot.as_mut().expect("snapshot").items[0].ticket_details[0].assignee =
            Some("matthias".into());
        state.snapshot.as_mut().expect("snapshot").items[4].ticket_details[0].assignee =
            Some("other".into());
        app.sidebar_work_filter.linear_statuses = [crate::app::state::LinearStatusFilter::Todo]
            .into_iter()
            .collect();

        let columns = ticket_board_columns(&app, &state);
        assert_eq!(columns[0].len(), 1);
        assert!(columns[4].is_empty(), "mine excludes another assignee");
        state.board_mine_only = false;
        let columns = ticket_board_columns(&app, &state);
        assert_eq!(
            columns[4].len(),
            1,
            "all includes Done despite sidebar active status filters"
        );
    }

    #[test]
    fn board_renders_compact_cards_and_tracks_nonzero_origin_spawn_geometry() {
        let app = AppState::test_new();
        let state = board_state();
        let layout = ticket_board_layout(&app, &state, Rect::new(26, 2, 93, 24));
        assert_eq!(layout.visible_columns, 0..5);
        assert_eq!(
            layout.cards.len(),
            4,
            "collapsed Done has no card hit areas"
        );
        assert_eq!(
            layout.spawn_buttons[0].1.x,
            layout.cards[0].rect.right().saturating_sub(3)
        );
        let text = rendered_text_at(&state, 120, 30);
        assert!(text.contains("Todo"), "{text}");
        assert!(text.contains("no agent"), "{text}");
        assert!(
            !text.contains("SCA-1 P2"),
            "status and left priority glyphs are removed: {text}"
        );
        assert!(
            text.contains("✓ 1"),
            "Done is collapsed to its count: {text}"
        );
    }

    fn board_app_with_local_agent(state: crate::detect::AgentState, seen: bool) -> AppState {
        let mut app = AppState::test_new();
        app.agent_host_name = "ub1".into();
        let workspace = crate::workspace::Workspace::test_new("board");
        let pane = workspace.tabs[0].panes.values().next().expect("pane");
        let id = pane.attached_terminal_id.clone();
        let mut terminal =
            crate::terminal::TerminalState::new(id.clone(), std::path::PathBuf::from("/tmp"));
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.set_detected_state(Some(crate::detect::Agent::Codex), state);
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["sca-3".into()]),
                ..Default::default()
            })
            .expect("context");
        app.terminals.insert(id, terminal);
        app.workspaces.push(workspace);
        for pane in app.workspaces[0].tabs[0].panes.values_mut() {
            pane.seen = seen;
        }
        app.nerd_font = false;
        app
    }

    #[test]
    fn board_projects_attached_local_agents_and_live_lifecycle() {
        for (state, seen, label) in [
            (crate::detect::AgentState::Working, true, "working"),
            (crate::detect::AgentState::Blocked, true, "blocked"),
            (crate::detect::AgentState::Idle, true, "idle"),
            (crate::detect::AgentState::Idle, false, "done"),
        ] {
            let mut app = board_app_with_local_agent(state, seen);
            app.work_view = Some(board_state());
            let summaries = board_agent_summaries(&app);
            assert_eq!(summaries.len(), 1);
            let ticket = &app
                .work_view
                .as_ref()
                .expect("view")
                .snapshot
                .as_ref()
                .expect("snapshot")
                .items[2]
                .ticket_details[0];
            let text = board_agent_line(&app, ticket, &summaries, 50).to_string();
            assert!(text.contains(&format!("cx ub1 · {label}")), "{text}");
            assert!(!text.contains("no agent") && !text.contains('+'), "{text}");
            let layout = ticket_board_layout(
                &app,
                app.work_view.as_ref().expect("view"),
                Rect::new(0, 0, 160, 24),
            );
            assert!(!layout
                .spawn_buttons
                .iter()
                .any(|(key, _)| key.ticket_id.as_deref() == Some("SCA-3")));
        }
    }

    #[test]
    fn board_attached_remote_agent_uses_remote_host_context_and_age() {
        let (mut app, _) = crate::ui::sidebar::tests::remote_control_fixture(
            crate::fleet::HostState::Reachable,
            crate::api::schema::AgentStatus::Working,
            false,
            false,
            false,
        );
        let mut info = app.fleet_snapshot.hosts[0].entries[0]
            .agent_info()
            .expect("info")
            .clone();
        info.work_context.ticket_ids = vec!["SCA-3".into()];
        info.reported_at = Some("1970-01-01T00:00:00Z".into());
        app.fleet_snapshot.hosts[0].entries[0] =
            crate::fleet::FleetRow::test_agent_info_row("ub2", info);
        app.remote_agent_panel_entries =
            crate::ui::sidebar::remote_agent_panel_entries_at(&app.fleet_snapshot, 2, false);
        let summaries = board_agent_summaries(&app);
        let remote = summaries.get("sca-3").expect("remote ticket agent");
        assert_eq!(remote.host, "ub2");
        assert_eq!(remote.state, crate::detect::AgentState::Working);
        assert!(remote.activity_at.is_some());
    }

    #[test]
    fn board_card_alignment_wrapping_fallback_and_clipped_controls() {
        let mut app = AppState::test_new();
        let state = board_state();
        let ticket = &state.snapshot.as_ref().expect("snapshot").items[0].ticket_details[0];
        let heading = board_card_heading(ticket, true, 24);
        assert_eq!(heading.width(), 24);
        assert!(heading.starts_with("▸SCA-1"));
        assert!(heading.ends_with("P2 "));
        assert_eq!(
            wrap_board_title("export presets for studio", 18),
            ["export presets for".to_string(), "studio".to_string()]
        );
        assert_eq!(
            wrap_board_title("averylongwordcannotfit and more", 10),
            ["averylongw".to_string(), "ordcannot…".to_string()]
        );
        for nerd_font in [true, false] {
            app.nerd_font = nerd_font;
            let layout = ticket_board_layout(&app, &state, Rect::new(26, 2, 120, 24));
            let card = &layout.cards[0];
            let line =
                board_agent_line(&app, ticket, &Default::default(), card.rect.width as usize);
            assert_eq!(line.width(), card.rect.width as usize);
            let plus = if nerd_font { "＋" } else { "+" };
            assert!(line.to_string().ends_with(&format!("{plus} ")));
            let button = layout.spawn_buttons[0].1;
            assert_eq!(button.x + button.width + 1, card.rect.right());
            assert_eq!(button.y, card.rect.y + 3);
            for width in 0..20 {
                let area = Rect::new(26, 2, width, 10);
                let layout = ticket_board_layout(&app, &state, area);
                for rect in [
                    layout.list_toggle,
                    layout.board_toggle,
                    layout.filter_toggle,
                ] {
                    assert!(rect.is_empty() || (rect.x >= area.x && rect.right() <= area.right()));
                }
            }
        }
    }

    #[test]
    fn board_mine_uses_viewer_identity_and_stale_scroll_matches_rendered_cards() {
        let mut app = AppState::test_new();
        app.work_index_session
            .set_linear_viewer_identity_for_test("viewer-id");
        let mut state = board_state();
        state.board_mine_only = true;
        state.snapshot.as_mut().expect("snapshot").items[2].ticket_details[0].assignee =
            Some("viewer-id".into());
        assert_eq!(ticket_board_columns(&app, &state)[2].len(), 1);
        state.board_scroll[2] = usize::MAX;
        let layout = ticket_board_layout(&app, &state, Rect::new(0, 0, 160, 24));
        assert_eq!(layout.cards.len(), 1);
        assert_eq!(layout.cards[0].row, 0);
        app.work_view = Some(state);
        assert!(rendered_app_text_at(&app, 160, 24).contains("SCA-3"));
    }

    #[test]
    fn board_done_detail_and_mouse_rectangles_follow_board_scope() {
        let mut app = AppState::test_new();
        let mut state = board_state();
        state.board_column = 4;
        state.selected = ticket_board_columns(&app, &state)[4].first().cloned();
        state.board_detail_open = true;
        app.work_view = Some(state);
        let detail = rendered_app_text_at(&app, 160, 40);
        assert!(
            detail.contains("SCA-5"),
            "Done detail must not fall back to an active ticket: {detail}"
        );
        app.work_view.as_mut().expect("view").board_detail_open = false;
        app.work_view.as_mut().expect("view").board_column = 0;
        let area = Rect::new(26, 2, 120, 24);
        let state = app.work_view.as_ref().expect("view");
        let layout = ticket_board_layout(&app, state, area);
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).expect("test terminal");
        terminal
            .draw(|frame| render_ticket_board(&app, state, area, frame))
            .expect("board render");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(layout.list_toggle.x, area.y)].symbol(), "L");
        assert_eq!(buffer[(layout.board_toggle.x, area.y)].symbol(), "B");
        assert_eq!(buffer[(layout.filter_toggle.x, area.y)].symbol(), "🔍");
        for (key, plus) in &layout.spawn_buttons {
            assert_eq!(buffer[(plus.x, plus.y)].symbol(), "＋");
            let card = layout
                .cards
                .iter()
                .find(|card| &card.key == key)
                .expect("card");
            assert_eq!(plus.y, card.rect.y + 3);
        }
        let done = layout.done_column.expect("Done strip");
        assert!(done.width < layout.column_areas[0].width);
        assert_eq!(buffer[(done.x + 2, done.y)].symbol(), "✓");
    }

    #[test]
    fn board_live_age_refreshes_with_sidebar_hidden() {
        let mut app = board_app_with_local_agent(crate::detect::AgentState::Working, true);
        let activity = app
            .terminals
            .values()
            .next()
            .expect("terminal")
            .agent_activity_at()
            .expect("activity");
        app.work_view = Some(board_state());
        app.view.terminal_area = Rect::new(0, 0, 160, 24);
        app.view.visible_agent_activity_instants.clear();
        let now = activity + std::time::Duration::from_secs(75);
        assert_eq!(
            app.next_agent_activity_age_change(now),
            Some(activity + std::time::Duration::from_secs(120))
        );
        app.work_view.as_mut().expect("view").board_detail_open = true;
        assert_eq!(app.next_agent_activity_age_change(now), None);
    }

    #[test]
    #[ignore = "supporting render scaling profile; run with just bench-render-scale"]
    fn board_render_scale_profile() {
        let mut measurements = Vec::new();
        for panes in [1, 15] {
            let mut app = board_app_with_local_agent(crate::detect::AgentState::Working, true);
            for _ in 1..panes {
                let mut extra =
                    board_app_with_local_agent(crate::detect::AgentState::Working, true);
                app.workspaces.append(&mut extra.workspaces);
                app.terminals.extend(extra.terminals);
            }
            app.work_view = Some(board_state());
            app.view.terminal_area = Rect::new(0, 0, 160, 40);
            let mut terminal = Terminal::new(TestBackend::new(160, 40)).expect("test terminal");
            for _ in 0..10 {
                terminal
                    .draw(|frame| render(&app, frame.area(), frame))
                    .expect("warmup");
            }
            let started = std::time::Instant::now();
            for _ in 0..200 {
                terminal
                    .draw(|frame| render(&app, frame.area(), frame))
                    .expect("board render");
                std::hint::black_box(app.next_agent_activity_age_change(app.view_observed_at));
            }
            let per_frame = started.elapsed().as_secs_f64() * 1_000_000.0 / 200.0;
            println!("board profile 160x40, {panes} populated panes: {per_frame:.2} us/frame");
            measurements.push(per_frame);
        }
        println!(
            "board profile 1 -> 15 panes: {:.2}x, {:+.2} us/frame",
            measurements[1] / measurements[0],
            measurements[1] - measurements[0]
        );
    }

    #[test]
    fn board_pages_two_columns_at_eighty_and_renders_all_at_one_twenty() {
        let app = AppState::test_new();
        let mut state = board_state();
        let narrow = ticket_board_layout(&app, &state, Rect::new(26, 2, 53, 20));
        assert_eq!(narrow.visible_columns, 0..2);
        state.board_column = 4;
        let last_page = ticket_board_layout(&app, &state, Rect::new(26, 2, 53, 20));
        assert_eq!(last_page.visible_columns, 3..5);
        let wide = ticket_board_layout(&app, &state, Rect::new(26, 2, 93, 36));
        assert_eq!(wide.visible_columns, 0..5);

        state.board_column = 0;
        let text = rendered_text_at(&state, 120, 40);
        assert!(text.contains("List | Board"), "{text}");
        assert!(text.contains("In Progress"), "{text}");
        assert!(
            text.contains("✓ 1"),
            "collapsed Done shows its count: {text}"
        );

        state.board_column = 4;
        let done_selected = ticket_board_layout(&app, &state, Rect::new(26, 2, 93, 36));
        assert!(done_selected.done_column.is_none());
        assert!(done_selected
            .cards
            .iter()
            .any(|card| card.key.ticket_id.as_deref() == Some("SCA-5")));

        state.refreshing = true;
        let narrow_text = rendered_text_at(&state, 80, 24);
        assert!(
            narrow_text.contains("Board   List | Board"),
            "{narrow_text}"
        );
    }

    #[test]
    fn successful_transition_reprojects_the_card_into_its_new_column() {
        let app = AppState::test_new();
        let mut state = board_state();
        let before = ticket_board_columns(&app, &state);
        assert!(before[2]
            .iter()
            .any(|key| key.ticket_id.as_deref() == Some("SCA-3")));
        let ticket = state
            .snapshot
            .as_mut()
            .and_then(|snapshot| snapshot.items.get_mut(2))
            .and_then(|item| item.ticket_details.first_mut())
            .expect("transitioned ticket");
        ticket.state = Some("Done".into());
        let after = ticket_board_columns(&app, &state);
        assert!(!after[2]
            .iter()
            .any(|key| key.ticket_id.as_deref() == Some("SCA-3")));
        assert!(after[4]
            .iter()
            .any(|key| key.ticket_id.as_deref() == Some("SCA-3")));
    }

    fn rendered_text(state: &WorkViewState) -> String {
        let mut app = AppState::test_new();
        app.work_view = Some(state.clone());
        rendered_app_text(&app)
    }

    fn rendered_text_at(state: &WorkViewState, width: u16, height: u16) -> String {
        let mut app = AppState::test_new();
        app.work_view = Some(state.clone());
        rendered_app_text_at(&app, width, height)
    }

    fn rendered_app_text(app: &AppState) -> String {
        rendered_app_text_at(app, 100, 12)
    }

    fn rendered_app_text_at(app: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(app, frame.area(), frame))
            .expect("render work view");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn pr_header_uses_merge_and_shared_overflow_actions() {
        let item = pr("owner/repo", 42, &[]);
        let key = crate::app::state::WorkItemKey {
            repo: item.repo.clone(),
            pr_number: item.pr_number,
            pr_url: item.pr_url.clone(),
            ticket_id: None,
        };
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.number = Some(42);
        detail.actions = vec![crate::work_index::WorkItemAction {
            name: "test".into(),
            state: "SUCCESS".into(),
        }];
        detail.merge_state_status = Some("CLEAN".into());
        detail.mergeable = Some("MERGEABLE".into());
        detail.head_sha = Some("abc123".into());
        let mut app = AppState::test_new();
        app.work_item_detail_cache.insert(key, detail);
        app.work_view = Some(WorkViewState::new(true, Some(snapshot(vec![item]))));

        let rendered = rendered_app_text_at(&app, 120, 24);
        assert!(rendered.contains("[Check out ▾] [Merge] [⋯]"), "{rendered}");

        app.work_view.as_mut().expect("work view").pr_action_menu = Some(Default::default());
        let menu = rendered_app_text_at(&app, 120, 40);
        assert!(menu.contains("Ask a question"), "{menu}");
        assert!(menu.contains("Close pull request"), "{menu}");
    }

    #[test]
    fn full_pr_action_menu_opens_downward_from_the_header() {
        let summary = pr("owner/repo", 42, &[]);
        let detail = crate::work_index::WorkItemDetail::empty();
        let actions = crate::ui::work_list_detail::PrItem {
            summary: &summary,
            cached_detail: Some(&detail),
            observed_at: SystemTime::UNIX_EPOCH,
        }
        .action_table(crate::config::MergeMethodConfig::Merge, true);
        let area = Rect::new(30, 12, 60, 20);
        let anchor = pr_action_menu_anchor(area, 4);
        let layout = crate::ui::pr_actions::layout(
            Rect::new(0, 0, 120, 40),
            anchor,
            &actions,
            Default::default(),
        )
        .expect("menu fits below the full-view header");
        assert_eq!(layout.rect.y, anchor.bottom());
    }

    #[test]
    fn timeline_tab_renders_cached_events_and_inline_failure() {
        let item = pr("owner/repo", 42, &[]);
        let key = crate::app::state::WorkItemKey {
            repo: item.repo.clone(),
            pr_number: item.pr_number,
            pr_url: item.pr_url.clone(),
            ticket_id: None,
        };
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.timeline = crate::work_index::parse_github_timeline(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/work-index/github-pr-timeline.json"
            ))
            .expect("timeline fixture"),
        );
        let mut app = AppState::test_new();
        app.work_item_detail_cache.insert(key.clone(), detail);
        let mut view = WorkViewState::new(true, Some(snapshot(vec![item])));
        view.selected = Some(key.clone());
        view.object_view_mut(key.clone()).tab = PrDetailTab::Timeline;
        app.work_view = Some(view);

        let rendered = rendered_app_text_at(&app, 120, 40);
        assert!(rendered.contains("Timeline · newest first"), "{rendered}");
        assert!(rendered.contains("First line of the comment"), "{rendered}");
        assert!(!rendered.contains("Second line is omitted"), "{rendered}");

        let mut failed = crate::work_index::WorkItemDetail::empty();
        failed.timeline_unavailable = Some("timeline unavailable".into());
        app.work_item_detail_cache.insert(key, failed);
        let rendered = rendered_app_text_at(&app, 120, 40);
        assert!(rendered.contains("timeline unavailable"), "{rendered}");
    }

    #[test]
    fn reviewer_picker_filters_and_opens_downward() {
        let anchor = Rect::new(80, 8, 4, 1);
        let layout = reviewer_picker_layout(Rect::new(26, 2, 94, 38), anchor, 12, 9)
            .expect("reviewer picker fits below row");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert!(layout.first_visible <= 9);
        assert!(9 < layout.first_visible + layout.visible_rows);

        let item = pr("owner/repo", 42, &[]);
        let key = crate::app::state::WorkItemKey {
            repo: item.repo.clone(),
            pr_number: item.pr_number,
            pr_url: item.pr_url.clone(),
            ticket_id: None,
        };
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.collaborators = vec!["ada".into(), "grace".into()];
        let mut app = AppState::test_new();
        app.work_item_detail_cache.insert(key, detail);
        let mut view = WorkViewState::new(true, Some(snapshot(vec![item])));
        let mut picker = crate::app::state::ReviewerPickerState::default();
        picker.filter.set_query("ad");
        view.reviewer_picker = Some(picker);
        app.work_view = Some(view);

        let rendered = rendered_app_text_at(&app, 120, 40);
        assert!(rendered.contains("Reviewers  —  ✦ +"), "{rendered}");
        assert!(rendered.contains("ad▏"), "{rendered}");
        assert!(rendered.contains("ada"), "{rendered}");
        assert!(!rendered.contains("grace"), "{rendered}");
    }

    #[test]
    fn disabled_not_collected_and_unavailable_render_safely() {
        assert!(rendered_text(&WorkViewState::new(false, None)).contains("work index disabled"));
        assert!(
            rendered_text(&WorkViewState::new(true, None)).contains("work index not yet collected")
        );
        assert!(rendered_text(&WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: Some(crate::work_index::WorkIndexUnavailable::only(
                    crate::work_index::WorkIndexSource::Github,
                    "observation failed",
                )),
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        ))
        .contains("GitHub: query failed (see log)"));

        let mut placeholder = WorkViewState::new(true, Some(snapshot(Vec::new())));
        placeholder.projection = WorkProjection::Tickets;
        assert!(rendered_text(&placeholder).contains("no matching tickets"));
    }

    #[test]
    fn review_queue_degraded_states_render_safely() {
        let mut disabled = WorkViewState::new(false, None);
        disabled.projection = WorkProjection::ReviewQueue;
        assert!(rendered_text(&disabled).contains("work index disabled"));

        let mut not_collected = WorkViewState::new(true, None);
        not_collected.projection = WorkProjection::ReviewQueue;
        assert!(rendered_text(&not_collected).contains("work index not yet collected"));

        let mut unavailable = WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: Some(crate::work_index::WorkIndexUnavailable::only(
                    crate::work_index::WorkIndexSource::Github,
                    "unavailable",
                )),
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        );
        unavailable.projection = WorkProjection::ReviewQueue;
        assert!(rendered_text(&unavailable).contains("query failed (see log)"));
    }

    #[test]
    fn review_queue_fixture_renders_all_verdicts_and_matching_counts() {
        let mut state = WorkViewState::new(
            true,
            Some(snapshot(vec![
                review_pr(
                    "scalablev2",
                    3226,
                    "ci(preview): allowlist",
                    &["SCA-2462"],
                    Some("In Progress"),
                ),
                review_pr(
                    "scalablev2",
                    3214,
                    "feat(image): restore prompt access",
                    &["SCA-2462", "SCA-2463", "SCA-2464"],
                    Some("In Progress"),
                ),
                review_pr(
                    "scalablev2",
                    3211,
                    "fix(onboarding): retry dispatch",
                    &[],
                    None,
                ),
                review_pr(
                    "scalablev2",
                    2531,
                    "fix(SCA-2462): renewal reconcile",
                    &["SCA-2462"],
                    Some("In Review"),
                ),
            ])),
        );
        state.projection = WorkProjection::ReviewQueue;
        state.repo_filter = Some("scalablev2".to_string());

        let text = rendered_text(&state);
        assert!(text.contains("work · review queue · scalablev2"));
        assert!(text.contains("awaiting review · 4"));
        assert!(text.contains("ticket says \"In Review\" · 1"));
        assert!(text.contains("3226"));
        assert!(text.contains("SCA-2462"));
        assert!(text.contains("3 tickets"));
        assert!(text.contains("In Progress"));
        assert!(text.contains("⚠ state drift"));
        assert!(text.contains("no ticket"));
        assert!(text.contains("⚠ untracked"));
        assert!(text.contains("In Review"));
        assert!(text.contains("✓"));
        assert!(text.contains("drift 3 PRs awaiting review whose ticket is not In Review"));
        assert!(text.contains("←/→ view PRs tickets Missive agents [review queue]"));
        assert!(text.contains("↑/↓ move"));
        assert!(text.contains("f filter repo"));
    }

    #[test]
    fn fixture_matches_required_group_and_footer_contract() {
        let text = rendered_text(&WorkViewState::new(
            true,
            Some(snapshot(vec![
                pr("scalablev2", 3226, &["SCA-2462"]),
                pr("scalablev2", 3244, &[]),
            ])),
        ));
        assert!(text.contains("Pull requests"));
        assert!(text.contains("search or label:bug"));
        assert!(text.contains("Others"));
        assert!(text.contains("#3226"));
        assert!(text.contains("#3244"));
        assert!(text.contains("Tab detail tabs"));
    }

    #[test]
    fn checkout_dropdown_opens_downward_and_clamps() {
        let area = Rect::new(40, 3, 50, 4);
        let layout =
            checkout_menu_layout(area, 0, crate::app::state::PrCheckoutChoice::NewWorktree)
                .expect("two rows fit below the action");
        assert_eq!(layout.rect.y, area.y + 1);
        assert_eq!(layout.visible_rows, 2);

        assert!(checkout_menu_layout(
            Rect::new(40, 6, 50, 1),
            0,
            crate::app::state::PrCheckoutChoice::CurrentCheckout,
        )
        .is_none());
    }

    #[test]
    fn ticket_fixture_renders_list_detail_and_refresh_state() {
        let mut state = WorkViewState::new(
            true,
            Some(snapshot(vec![
                ticket("SCA-3165", crate::work_index::TicketGroup::Assigned),
                ticket("SCA-3180", crate::work_index::TicketGroup::Triage),
            ])),
        );
        state.projection = WorkProjection::Tickets;
        state.refreshing = true;
        let text = rendered_text_at(&state, 100, 24);
        assert!(text.contains("Tickets · refreshing…"), "{text}");
        assert!(text.contains("Assigned to me"), "{text}");
        assert!(text.contains("Triage"), "{text}");
        assert!(text.contains("SCA-3165"), "{text}");
        assert!(
            text.contains("In Progress · P2 · matthias · cycle 34"),
            "{text}"
        );
        assert!(text.contains("Ticket body with"), "{text}");
        assert!(text.contains("https://example.invalid"), "{text}");
        assert!(text.contains("Acceptance criteria"), "{text}");
        assert!(text.contains("[Start thread ▾] [⋯]"), "{text}");
    }

    #[test]
    fn full_ticket_action_menu_uses_shared_rows_below_header() {
        let mut state = WorkViewState::new(
            true,
            Some(snapshot(vec![ticket(
                "SCA-3165",
                crate::work_index::TicketGroup::Assigned,
            )])),
        );
        state.projection = WorkProjection::Tickets;
        state.ticket_more_menu = Some(Default::default());
        let text = rendered_text_at(&state, 120, 40);
        for label in [
            "Refresh",
            "Ask a question",
            "Explain this ticket",
            "Transition ▸",
            "Priority ▸",
            "Copy identifier",
            "Cancel ticket",
        ] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
    }

    #[test]
    fn full_ticket_action_menu_opens_downward_and_clamps() {
        let item = ticket("SCA-3165", crate::work_index::TicketGroup::Assigned);
        let ticket = item.ticket_details.first().expect("ticket fixture");
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            ticket, None, None, None, false,
        );
        let area = Rect::new(40, 3, 50, 5);
        let anchor = ticket_action_menu_anchor(area, 1);
        let layout = crate::ui::ticket_actions::ticket_action_menu_layout(
            anchor,
            area,
            &context,
            crate::ui::ticket_actions::TicketActionMenuState {
                selected: 12,
                ..Default::default()
            },
        )
        .expect("rows below the ticket header");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert_eq!(layout.rect.bottom(), area.bottom());
        assert!(layout.first_visible <= 12);
        assert!(12 < layout.first_visible + layout.visible_rows);
    }

    #[test]
    fn github_degradation_does_not_hide_linear_items() {
        let mut degraded = snapshot(vec![ticket(
            "SCA-3165",
            crate::work_index::TicketGroup::Assigned,
        )]);
        degraded.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Github,
            "rate limited",
        ));
        let mut state = WorkViewState::new(true, Some(degraded));
        state.projection = WorkProjection::Tickets;

        let text = rendered_text_at(&state, 100, 24);
        assert!(text.contains("SCA-3165"), "{text}");
        assert!(!text.contains("GitHub: rate limited"), "{text}");
    }

    #[test]
    fn degraded_sources_keep_their_previous_rows_visible() {
        let mut github_snapshot = snapshot(vec![pr("owner/repo", 77, &[])]);
        github_snapshot.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Github,
            "rate limited",
        ));
        let github = rendered_text_at(&WorkViewState::new(true, Some(github_snapshot)), 100, 24);
        assert!(github.contains("GitHub: rate limited"), "{github}");
        assert!(github.contains("PR 77"), "{github}");

        let mut linear_snapshot = snapshot(vec![ticket(
            "SCA-3165",
            crate::work_index::TicketGroup::Assigned,
        )]);
        linear_snapshot.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Linear,
            "rate limited",
        ));
        let mut linear_state = WorkViewState::new(true, Some(linear_snapshot));
        linear_state.projection = WorkProjection::Tickets;
        let linear = rendered_text_at(&linear_state, 100, 24);
        assert!(linear.contains("Linear: rate limited"), "{linear}");
        assert!(linear.contains("SCA-3165"), "{linear}");

        let mut missive_snapshot = snapshot(Vec::new());
        missive_snapshot.conversations = vec![missive_conversation(1)];
        missive_snapshot.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Missive,
            "rate limited",
        ));
        let mut missive_state = WorkViewState::new(true, Some(missive_snapshot));
        missive_state.projection = WorkProjection::Missive;
        let missive = rendered_text_at(&missive_state, 100, 24);
        assert!(missive.contains("Missive: rate limited"), "{missive}");
        assert!(missive.contains("Billing question"), "{missive}");
    }

    #[test]
    fn ticket_dropdowns_open_downward_and_clamp() {
        let area = Rect::new(10, 4, 60, 8);
        for (anchor_x, count, selected, width) in [(12, 2, 1, 42), (29, 4, 3, 20), (54, 3, 2, 20)] {
            let layout = ticket_menu_layout(area, anchor_x, 0, count, selected, width)
                .expect("ticket menu fits below header");
            assert_eq!(layout.rect.y, area.y + 1);
            assert!(layout.rect.bottom() <= area.bottom());
        }
        assert!(ticket_menu_layout(Rect::new(0, 2, 40, 1), 1, 0, 2, 0, 20).is_none());
    }

    #[test]
    fn missive_fixture_renders_list_detail_actions_and_footer() {
        let conversation = crate::work_index::MissiveConversation {
            id: "sample".into(),
            subject: "Billing question".into(),
            app_url: "missive://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            team: None,
            assignees: vec![crate::work_index::MissiveUser {
                id: "ada".into(),
                name: "Ada".into(),
                email: None,
                is_me: true,
            }],
            last_activity_at: Some(SystemTime::UNIX_EPOCH),
            closed: false,
            labels: Vec::new(),
            pane_bound: false,
            messages: vec![crate::work_index::MissiveEntry {
                id: "message".into(),
                author: Some("Customer".into()),
                preview: "invoice preview".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            }],
            notes: vec![crate::work_index::MissiveEntry {
                id: "note".into(),
                author: Some("Ada".into()),
                preview: "internal note".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            }],
            drafts: Vec::new(),
            posts: Vec::new(),
        };
        let mut state = WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                conversations: vec![conversation],
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        );
        state.projection = WorkProjection::Missive;
        let text = rendered_app_text_at(
            &AppState {
                work_view: Some(state),
                ..AppState::test_new()
            },
            120,
            40,
        );
        assert!(text.contains("Missive"), "{text}");
        assert!(text.contains("Billing question"), "{text}");
        assert!(text.contains("Customer · 0m ago"), "{text}");
        assert!(text.contains("invoice preview"), "{text}");
        assert!(text.contains("Internal notes  1"), "{text}");
        assert!(
            text.contains("[Start thread ▾] [Open in Missive]"),
            "{text}"
        );
        assert!(text.contains("o Open in Missive"), "{text}");
    }

    #[test]
    fn missive_full_view_at_eighty_columns_keeps_subject_actions_and_scroll_hint() {
        let text = rendered_text_at(&missive_state(8), 80, 24);
        assert!(text.contains("Billing question"), "{text}");
        assert!(
            text.contains("[Start thread ▾] [Open in Missive]"),
            "{text}"
        );
        assert!(text.contains("PgUp/PgDn detail"), "{text}");
        assert!(text.contains("o copy"), "{text}");
    }

    #[test]
    fn missive_detail_scroll_reaches_later_messages_independently() {
        let mut state = missive_state(10);
        let initial = rendered_text_at(&state, 80, 24);
        assert!(initial.contains("message body 0"), "{initial}");
        assert!(!initial.contains("message body 6"), "{initial}");

        state.missive_detail_scroll = 13;
        let scrolled = rendered_text_at(&state, 80, 24);
        assert!(scrolled.contains("message body 6"), "{scrolled}");
        assert!(!scrolled.contains("message body 0"), "{scrolled}");

        state.missive_detail_scroll = u16::MAX;
        let clamped = rendered_text_at(&state, 80, 24);
        assert!(clamped.contains("message body 9"), "{clamped}");
    }

    #[test]
    fn missive_controls_fit_each_responsive_layout() {
        for width in [96, 100, 128] {
            let text = rendered_text_at(&missive_state(1), width, 24);
            assert!(text.contains("⚲ open"), "width {width}: {text}");
            assert!(text.contains("r refresh"), "width {width}: {text}");
            assert!(!text.contains("teammates"), "width {width}: {text}");
        }
    }

    #[test]
    fn missive_list_status_glyph_uses_palette_state_colour() {
        let state = missive_state(1);
        let app = AppState {
            work_view: Some(state),
            ..AppState::test_new()
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(&app, frame.area(), frame))
            .expect("render Missive view");
        let glyph = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .find(|cell| cell.symbol() == "○")
            .expect("open conversation glyph");
        assert_eq!(glyph.fg, app.palette.work_status_open());
    }

    #[test]
    fn missive_start_dropdown_opens_downward_and_clamps() {
        let area = Rect::new(20, 3, 50, 4);
        let layout = ticket_menu_layout(area, 12, 0, 2, 1, 24).expect("menu fits below anchor");
        assert_eq!(layout.rect.y, area.y + 1);
        assert!(layout.rect.bottom() <= area.bottom());
        assert!(ticket_menu_layout(Rect::new(0, 2, 40, 1), 12, 0, 2, 0, 24).is_none());
    }

    #[test]
    fn missive_view_distinguishes_collecting_failure_and_empty_states() {
        let mut state = WorkViewState::new(true, None);
        state.projection = WorkProjection::Missive;
        let collecting = rendered_text(&state);
        assert!(
            collecting.contains("work index not yet collected"),
            "{collecting}"
        );

        state.snapshot = Some(Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(crate::work_index::WorkIndexUnavailable::only(
                crate::work_index::WorkIndexSource::Missive,
                "observation timed out",
            )),
            observed_at: SystemTime::UNIX_EPOCH,
        });
        let failed = rendered_text(&state);
        assert!(failed.contains("Missive: timed out (30s)"), "{failed}");

        state.snapshot.as_mut().expect("snapshot").unavailable = None;
        let empty = rendered_text(&state);
        assert!(empty.contains("no matching conversations"), "{empty}");
    }

    fn pr_layout_fixture(view: &ObjectViewState) -> DetailLayout {
        let summary = pr("owner/repo", 206, &[]);
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.number = Some(206);
        detail.title = Some("rich dock view".into());
        detail.body = Some("A description that has to disappear when folded.".into());
        detail.actions = vec![crate::work_index::WorkItemAction {
            name: "tests".into(),
            state: "SUCCESS".into(),
        }];
        let item = crate::ui::work_list_detail::PrItem {
            summary: &summary,
            cached_detail: Some(&detail),
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let app = AppState::test_new();
        pr_detail_layout(
            &app,
            &item,
            view,
            &PrDetailControls::default(),
            Rect::new(0, 0, 72, 24),
        )
    }

    fn layout_text(layout: &DetailLayout) -> String {
        layout
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_collapsed_section_drops_its_body_and_keeps_its_header() {
        let expanded = pr_layout_fixture(&ObjectViewState::default());
        assert!(
            layout_text(&expanded).contains("has to disappear when folded"),
            "{}",
            layout_text(&expanded)
        );

        let mut view = ObjectViewState::default();
        view.toggle_section(crate::app::state::DetailSection::Description);
        let collapsed = pr_layout_fixture(&view);
        let text = layout_text(&collapsed);
        assert!(!text.contains("has to disappear when folded"), "{text}");
        assert!(text.contains("Description"), "the header stays: {text}");
        assert!(
            collapsed.lines.len() < expanded.lines.len(),
            "folding has to reclaim rows"
        );
        assert_eq!(
            collapsed.sections.len(),
            expanded.sections.len(),
            "a folded section is still foldable, so it keeps its digit"
        );
    }

    #[test]
    fn every_section_header_index_points_at_its_own_rule() {
        let layout = pr_layout_fixture(&ObjectViewState::default());
        assert!(!layout.sections.is_empty());
        for (header, _) in &layout.sections {
            let text = layout.lines[*header]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(
                text.starts_with('\u{2500}'),
                "line {header} is not a section rule: {text}"
            );
        }
    }

    #[test]
    fn ticket_section_digits_stay_contiguous_without_acceptance_criteria() {
        let ticket = crate::work_index::WorkTicket {
            identifier: "ENG-1".into(),
            title: Some("a ticket".into()),
            description: Some("a body".into()),
            state: Some("In Progress".into()),
            assignee: None,
            creator: None,
            priority: None,
            cycle: None,
            group: crate::work_index::TicketGroup::default(),
            created_at: None,
            updated_at: None,
            branch: None,
            labels: Vec::new(),
            url: None,
            parent: None,
            relations: Vec::new(),
        };
        let app = AppState::test_new();
        let view = ObjectViewState::default();
        // A ticket's acceptance criteria are the checklist in its description,
        // so a description without one drops the section entirely.
        let sections = |description: &str| {
            let ticket = crate::work_index::WorkTicket {
                description: Some(description.into()),
                ..ticket.clone()
            };
            let item = crate::ui::work_list_detail::TicketItem {
                summary: &ticket,
                cached_detail: None,
                linked_prs: Vec::new(),
                observed_at: SystemTime::UNIX_EPOCH,
                has_context_pr: false,
            };
            ticket_detail_layout(
                &app,
                &item,
                &view,
                &TicketDetailControls::default(),
                Rect::new(0, 0, 72, 24),
            )
            .sections
            .iter()
            .map(|(_, section)| *section)
            .collect::<Vec<_>>()
        };

        use crate::app::state::DetailSection;
        assert_eq!(
            sections("a body\n- [x] ships\n- [ ] left"),
            vec![
                DetailSection::LinkedPrs,
                DetailSection::Description,
                DetailSection::AcceptanceCriteria,
                DetailSection::Comments,
            ]
        );
        // With no acceptance criteria the section is absent entirely, so alt+3
        // names comments rather than leaving a hole in the numbering.
        assert_eq!(
            sections("a body with no checklist"),
            vec![
                DetailSection::LinkedPrs,
                DetailSection::Description,
                DetailSection::Comments,
            ]
        );
    }

    fn render_pr_detail_fixture(tab: PrDetailTab, width: u16) -> String {
        let summary = pr("owner/repo", 206, &[]);
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.number = Some(206);
        detail.title = Some("rich dock view".into());
        detail.body = Some("The full description stays available in either host.".into());
        detail.base_ref_name = Some("main".into());
        detail.head_ref_name = Some("feature/rich-dock".into());
        detail.files = vec![crate::work_index::WorkItemFile {
            path: "src/ui/work_view.rs".into(),
            additions: 21,
            deletions: 4,
        }];
        detail.actions = vec![crate::work_index::WorkItemAction {
            name: "tests".into(),
            state: "SUCCESS".into(),
        }];
        detail.timeline = vec![crate::work_index::WorkItemTimelineEvent {
            kind: "reviewed".into(),
            actor: Some("matthias".into()),
            summary: "approved".into(),
            created_at: Some(SystemTime::UNIX_EPOCH),
        }];
        let item = crate::ui::work_list_detail::PrItem {
            summary: &summary,
            cached_detail: Some(&detail),
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let mut app = AppState::test_new();
        let diff_key = crate::app::state::DiffCacheKey {
            root: std::path::PathBuf::from("/repo"),
            base: "main".into(),
            ignore_whitespace: false,
        };
        app.dock_diff_cache.insert(
            diff_key.clone(),
            crate::app::state::DiffCacheEntry {
                branch: "feature/rich-dock".into(),
                files: vec![crate::app::state::DiffFileSummary {
                    path: "src/ui/work_view.rs".into(),
                    display_path: "src/ui/work_view.rs".into(),
                    additions: 1,
                    deletions: 1,
                    binary: false,
                }],
                contents: std::collections::HashMap::from([(
                    "src/ui/work_view.rs".into(),
                    crate::app::state::DiffFileContent {
                        committed: vec![crate::app::diff::DiffLine {
                            text: "@@ -1 +1 @@".into(),
                            kind: crate::app::diff::DiffLineKind::Hunk,
                        }],
                        uncommitted: Vec::new(),
                    },
                )]),
                error: None,
            },
        );
        app.dock_diff_active_key = Some(diff_key);
        let backend = TestBackend::new(width, 24);
        let mut terminal = Terminal::new(backend).expect("detail terminal");
        terminal
            .draw(|frame| {
                render_pr_detail(
                    &app,
                    &item,
                    &ObjectViewState {
                        tab,
                        ..Default::default()
                    },
                    PrDetailControls::default(),
                    frame.area(),
                    frame,
                )
            })
            .expect("render rich PR detail");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn one_pr_detail_renderer_drives_full_and_dock_hosts() {
        let full_host = render_pr_detail_fixture(PrDetailTab::Overview, 72);
        let dock_host = render_pr_detail_fixture(PrDetailTab::Overview, 72);
        assert_eq!(full_host, dock_host);
        assert!(full_host.contains("full description"), "{full_host}");
    }

    #[test]
    fn rich_pr_detail_renders_every_sub_tab() {
        for (tab, marker) in [
            (PrDetailTab::Overview, "Description"),
            (PrDetailTab::Files, "src/ui/work_view.rs"),
            (PrDetailTab::Diff, "@@ -1 +1 @@"),
            (PrDetailTab::Checks, "tests  SUCCESS"),
            (PrDetailTab::Timeline, "approved"),
        ] {
            let rendered = render_pr_detail_fixture(tab, 72);
            assert!(rendered.contains(marker), "{tab:?}: {rendered}");
        }
    }

    #[test]
    fn narrow_pr_detail_collapses_tabs_and_wraps_actions() {
        let app = AppState::test_new();
        let tab_row = pr_tab_row(&app.palette, PrDetailTab::Checks, 59, "✓ 1 of 1 passing");
        let text = tab_row
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("[Checks ▾]"), "{text}");
        assert!(!text.contains("[Overview]"), "{text}");

        let mut rows = Vec::new();
        push_pr_action_rows(
            &mut rows,
            &app.palette,
            "Check out ▾",
            "Merge",
            Style::default(),
            59,
        );
        assert_eq!(rows.len(), 2);
        let actions = rows
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for label in ["Check out ▾", "Merge", "⋯"] {
            assert!(actions.contains(label), "{actions}");
        }

        let mut ticket_rows = Vec::new();
        push_ticket_action_rows(&mut ticket_rows, &app.palette, 59);
        assert_eq!(ticket_rows.len(), 2);
        let ticket_actions = ticket_rows
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for label in ["Start thread ▾", "⋯"] {
            assert!(ticket_actions.contains(label), "{ticket_actions}");
        }
    }

    #[test]
    fn ticket_controls_hit_test_the_labels_the_detail_drew() {
        let area = Rect::new(10, 5, 80, 20);
        let start_width =
            u16::try_from(crate::ui::text::display_width(TICKET_START_LABEL)).expect("narrow");
        let row = area.y + 1;
        let start = area.x + 1;
        let more = start + start_width + 1;

        assert_eq!(
            ticket_action_control_at(area, 1, 0, start, row),
            Some(TicketActionControl::StartThread)
        );
        assert_eq!(
            ticket_action_control_at(area, 1, 0, more, row),
            Some(TicketActionControl::More)
        );
        // The gap between the two buttons belongs to neither.
        assert_eq!(
            ticket_action_control_at(area, 1, 0, start + start_width, row),
            None
        );
        // The heading above them is not a control.
        assert_eq!(ticket_action_control_at(area, 1, 0, start, area.y), None);
        // A narrow detail stacks them, and the scroll moves both.
        assert_eq!(
            ticket_action_control_at(area, 2, 0, start, row + 1),
            Some(TicketActionControl::More)
        );
        assert_eq!(
            ticket_action_control_at(area, 1, 1, start, area.y),
            Some(TicketActionControl::StartThread)
        );
    }

    #[test]
    fn narrow_pr_tab_picker_opens_downward_and_clamps() {
        let area = Rect::new(20, 3, 48, 9);
        let layout = pr_tab_picker_layout(area, 5, 4).expect("picker fits below tab row");
        assert_eq!(layout.rect.y, area.y + 6);
        assert!(layout.rect.bottom() <= area.bottom());
        assert!(layout.first_visible <= 4);
        assert!(4 < layout.first_visible + layout.visible_rows);
    }
}
