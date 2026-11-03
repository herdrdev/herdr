use std::collections::HashMap;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Paragraph, Widget},
};

use super::*;

pub(super) struct AgentRow {
    pub(super) pane_id: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) focused: bool,
    pub(super) rows: Vec<Vec<crate::ui::ResolvedToken>>,
    pub(super) shepherd_state: Option<String>,
    pub(super) needs_you: bool,
    pub(super) notice: Option<String>,
}

pub(super) fn ordered_agent_pane_ids(
    snapshot: &ClientShellSnapshot,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<String> {
    if snapshot.agent_view_label.is_some() {
        return snapshot
            .agent_order
            .iter()
            .filter(|pane_id| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane_id.as_str())
            })
            .cloned()
            .collect();
    }
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    if sort == crate::config::AgentPanelSortConfig::Priority {
        agents.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

pub(super) fn render_agent_panel(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if !render_agent_panel_header(
        buffer,
        area,
        snapshot.agent_view_label.as_deref(),
        config,
        hits,
    ) {
        return;
    }

    let rows = agent_rows(snapshot, config, None);
    render_agent_list(
        buffer,
        area,
        &rows,
        snapshot
            .agent_view_label
            .as_ref()
            .map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| agent_row_line_count(row, config),
        |buffer, rect, row, hits| {
            hits.agents.push((rect, row.pane_id.clone()));
            render_agent_row(buffer, rect, row, config);
        },
    );
}

pub(super) fn agent_row_line_count(row: &AgentRow, config: &ClientShellConfig) -> usize {
    let mut count = row.rows.len().max(1);
    if config.shepherd_enabled && row.notice.is_some() {
        count += 1;
    }
    count
}

pub(super) fn render_agent_panel_header(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) -> bool {
    if area.height == 0 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(config.palette.surface_dim),
    );
    if area.height < 2 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " agents",
        Style::default()
            .fg(config.palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let sort_label = agent_view_label.unwrap_or(match config.agent_panel_sort {
        crate::config::AgentPanelSortConfig::Spaces => "grouped",
        crate::config::AgentPanelSortConfig::Priority => "priority",
    });
    let sort_width = display_width(sort_label).min(area.width as usize) as u16;
    let sort_rect = Rect::new(
        area.right().saturating_sub(sort_width),
        area.y + 1,
        sort_width,
        1,
    );
    hits.agent_sort_toggle = if config.mouse_capture && agent_view_label.is_none() {
        sort_rect
    } else {
        Rect::default()
    };
    put_text(
        buffer,
        sort_rect.x,
        sort_rect.y,
        sort_rect.width,
        sort_label,
        Style::default()
            .fg(if agent_view_label.is_some() {
                config.palette.accent
            } else {
                config.palette.overlay0
            })
            .add_modifier(Modifier::BOLD),
    );
    true
}

pub(super) fn render_agent_list<T>(
    buffer: &mut Buffer,
    area: Rect,
    rows: &[T],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
    row_lines: impl Fn(&T) -> usize,
    mut render_row: impl FnMut(&mut Buffer, Rect, &T, &mut ShellHitMap),
) {
    let body = Rect::new(
        area.x,
        area.y.saturating_add(3),
        area.width,
        area.height.saturating_sub(3),
    );
    hits.agent_body = body;
    if body.is_empty() || rows.is_empty() {
        *agent_scroll = 0;
        if let Some(message) = empty_message.filter(|_| !body.is_empty()) {
            put_text(
                buffer,
                body.x,
                body.y,
                body.width,
                message,
                Style::default()
                    .fg(config.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            );
        }
        return;
    }

    let row_heights = rows
        .iter()
        .map(|row| row_lines(row).max(1).min(u16::MAX as usize) as u16)
        .collect::<Vec<_>>();
    let gaps = rows
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    let metrics =
        super::scroll::list_scroll_metrics(&row_heights, &gaps, body.height, *agent_scroll);
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (index, row) in rows.iter().enumerate().skip(*agent_scroll) {
        let height = row_heights[index].min(body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, height);
        render_row(buffer, rect, row, hits);
        y = y
            .saturating_add(height)
            .saturating_add(if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            });
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, &config.palette);
    }
}

pub(super) fn agent_rows(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Vec<AgentRow> {
    ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .filter_map(|pane_id| agent_row(snapshot, &pane_id, config, machine))
        .collect()
}

pub(super) fn agent_row(
    snapshot: &ClientShellSnapshot,
    pane_id: &str,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Option<AgentRow> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    let tab_count = snapshot
        .tabs
        .iter()
        .filter(|candidate| candidate.workspace_id == agent.workspace_id)
        .count();
    let tab_label = tab
        .filter(|tab| tab_count > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str());
    let agent_label = agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref());
    let labels = agent
        .state_labels
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let tokens = agent.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let state_text = labels
        .get(status_text(agent.agent_status))
        .map(String::as_str)
        .unwrap_or_else(|| sidebar_status_text(agent.agent_status));
    let canonical_agent = agent
        .agent
        .as_deref()
        .and_then(crate::detect::parse_agent_label);
    let rows = crate::ui::sidebar_agent_rows(
        &config.agents,
        crate::ui::AgentTokenContext {
            machine,
            workspace: &workspace.label,
            tab: tab_label,
            pane: agent
                .title
                .as_deref()
                .or_else(|| pane.and_then(|pane| pane.label.as_deref())),
            agent_label,
            terminal_title: agent.terminal_title.as_deref(),
            terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
            canonical_agent,
            tokens: &tokens,
        },
        state_text,
    );
    let (shepherd_state, needs_you, notice) = if let Some(ref shep) = agent.shepherd {
        let is_blocked = shep.state == "blocked";
        (
            Some(shep.state.clone()),
            is_blocked,
            shep.notice.clone(),
        )
    } else {
        (None, false, None)
    };
    Some(AgentRow {
        pane_id: agent.pane_id.clone(),
        status: agent.agent_status,
        focused: agent.focused,
        rows,
        shepherd_state,
        needs_you,
        notice,
    })
}

pub(super) fn render_agent_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &AgentRow,
    config: &ClientShellConfig,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let palette = &config.palette;
    let row_style = if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let name_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(status_color(row.status, palette));
    let secondary = Style::default().fg(palette.overlay0);
    let icon = (
        status_icon(row.status, config.status_indicators),
        Style::default().fg(status_color(row.status, palette)),
    );
    let rows = if row.rows.is_empty() {
        vec![vec![crate::ui::ResolvedToken {
            kind: crate::ui::ResolvedTokenKind::StateIcon,
            style: Default::default(),
        }]]
    } else {
        row.rows.clone()
    };
    for (index, tokens) in rows.iter().take(rect.height as usize).enumerate() {
        let indent = if index == 0 { 1 } else { 3 };
        let mut spans = vec![ratatui::text::Span::raw(" ".repeat(indent))];
        spans.extend(crate::ui::resolved_token_spans(
            tokens,
            icon,
            status_style,
            name_style,
            secondary,
            secondary,
            palette,
            rect.width.saturating_sub(indent as u16) as usize,
        ));
        if config.shepherd_enabled && index == 0 {
            if row.needs_you {
                spans.push(ratatui::text::Span::styled(
                    " [NEEDS YOU]",
                    Style::default().fg(palette.red).add_modifier(Modifier::BOLD),
                ));
            } else if let Some(ref state) = row.shepherd_state {
                spans.push(ratatui::text::Span::styled(
                    format!(" [{state}]"),
                    status_style.add_modifier(Modifier::DIM),
                ));
            }
            if rect.height <= rows.len() as u16 {
                if let Some(ref notice) = row.notice {
                    spans.push(ratatui::text::Span::styled(
                        format!(" ({notice})"),
                        Style::default().fg(palette.yellow),
                    ));
                }
            }
        }
        Paragraph::new(Line::from(spans)).style(row_style).render(
            Rect::new(rect.x, rect.y + index as u16, rect.width, 1),
            buffer,
        );
    }
    if config.shepherd_enabled && rect.height > rows.len() as u16 {
        if let Some(ref notice) = row.notice {
            let notice_y = rect.y + rows.len() as u16;
            let spans = vec![
                ratatui::text::Span::raw("   ↳ "),
                ratatui::text::Span::styled(
                    notice.as_str(),
                    Style::default().fg(palette.yellow),
                ),
            ];
            Paragraph::new(Line::from(spans)).style(row_style).render(
                Rect::new(rect.x, notice_y, rect.width, 1),
                buffer,
            );
        }
    }
}

fn put_text(buffer: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    for (offset, character) in text.chars().take(width as usize).enumerate() {
        if let Some(cell) = buffer.cell_mut((x + offset as u16, y)) {
            cell.set_char(character).set_style(style);
        }
    }
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn sidebar_status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Unknown => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use crate::config::Config;

    fn test_config(shepherd_enabled: bool) -> ClientShellConfig {
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.shepherd_enabled = shepherd_enabled;
        config
    }

    fn test_snapshot_with_agent(
        shepherd: Option<crate::api::schema::AgentShepherdInfo>,
        agent_status: crate::api::schema::AgentStatus,
    ) -> ClientShellSnapshot {
        let mut snapshot = crate::client::shell::tests::snapshot();
        snapshot.workspaces[0].label = "main".into();
        snapshot.tabs[0].label = "tab1".into();
        snapshot.agents = vec![crate::protocol::ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("coder-1".into()),
            display_agent: Some("coder".into()),
            agent: Some("coder".into()),
            title: Some("my task".into()),
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
            shepherd,
        }];
        snapshot
    }

    fn buffer_text(buffer: &Buffer, area: Rect) -> String {
        buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_shepherd_agent_in_each_state() {
        let states = [
            ("starting", "[starting]"),
            ("working", "[working]"),
            ("idle", "[idle]"),
            ("blocked", "[NEEDS YOU]"),
            ("done", "[done]"),
            ("exited", "[exited]"),
            ("killed", "[killed]"),
            ("failed", "[failed]"),
        ];

        let config = test_config(true);

        for (state, expected_marker) in states {
            let shepherd_info = crate::api::schema::AgentShepherdInfo {
                id: "coder-1".into(),
                role: "coder".into(),
                state: state.into(),
                notice: None,
            };
            let snapshot = test_snapshot_with_agent(Some(shepherd_info), crate::api::schema::AgentStatus::Working);
            let row = agent_row(&snapshot, "pane_1", &config, None).expect("agent row should exist");

            let area = Rect::new(0, 0, 80, 1);
            let mut buffer = Buffer::empty(area);
            render_agent_row(&mut buffer, area, &row, &config);

            let text = buffer_text(&buffer, area);
            assert!(
                text.contains(expected_marker),
                "Expected state '{state}' to render marker '{expected_marker}', got text: {text}"
            );
        }
    }

    #[test]
    fn test_shepherd_agent_blocked_with_notice() {
        let config = test_config(true);
        let notice_text = "waiting for human: trust dialog; open tab";
        let shepherd_info = crate::api::schema::AgentShepherdInfo {
            id: "coder-1".into(),
            role: "coder".into(),
            state: "blocked".into(),
            notice: Some(notice_text.into()),
        };
        let snapshot = test_snapshot_with_agent(Some(shepherd_info), crate::api::schema::AgentStatus::Blocked);
        let row = agent_row(&snapshot, "pane_1", &config, None).expect("agent row should exist");

        // Check row line count calculation
        let total_lines = agent_row_line_count(&row, &config);
        assert_eq!(total_lines, row.rows.len() + 1);

        // 1. Extra line available when total_lines allocated (dedicated extra line for notice)
        let area_extra = Rect::new(0, 0, 80, total_lines as u16);
        let mut buffer_extra = Buffer::empty(area_extra);
        render_agent_row(&mut buffer_extra, area_extra, &row, &config);
        let text_extra = buffer_text(&buffer_extra, area_extra);

        assert!(text_extra.contains("[NEEDS YOU]"), "Line 0 should show [NEEDS YOU]");
        assert!(text_extra.contains(notice_text), "Extra line should show full notice text");
        assert!(text_extra.contains("↳"), "Extra line should show subline indicator");

        // 2. Constrained height (inline notice on line 0)
        let area_inline = Rect::new(0, 0, 80, 1);
        let mut buffer_inline = Buffer::empty(area_inline);
        render_agent_row(&mut buffer_inline, area_inline, &row, &config);
        let text_inline = buffer_text(&buffer_inline, area_inline);

        assert!(text_inline.contains("[NEEDS YOU]"), "Constrained height should show [NEEDS YOU]");
        assert!(text_inline.contains(notice_text), "Constrained height should contain notice text");
    }

    #[test]
    fn test_non_shepherd_agent_blocked_without_marker() {
        let config = test_config(true);
        // An external/normal agent detected as Blocked by Herdr, but NOT a Shepherd agent (shepherd: None)
        let snapshot = test_snapshot_with_agent(None, crate::api::schema::AgentStatus::Blocked);
        let row = agent_row(&snapshot, "pane_1", &config, None).expect("agent row should exist");

        assert!(!row.needs_you, "Non-shepherd agent must never have needs_you true");
        assert_eq!(row.shepherd_state, None, "Non-shepherd agent must have shepherd_state None");

        let area = Rect::new(0, 0, 80, 2);
        let mut buffer = Buffer::empty(area);
        render_agent_row(&mut buffer, area, &row, &config);
        let text = buffer_text(&buffer, area);

        assert!(!text.contains("[NEEDS YOU]"), "Non-shepherd agent must NOT display [NEEDS YOU]");
        assert!(!text.contains("[blocked]"), "Non-shepherd agent must NOT display [blocked]");
    }

    #[test]
    fn test_characterization_normal_herdr_agent_renders_identically() {
        let config_disabled = test_config(false);
        let config_enabled = test_config(true);

        let snapshot = test_snapshot_with_agent(None, crate::api::schema::AgentStatus::Working);
        let row_disabled = agent_row(&snapshot, "pane_1", &config_disabled, None).unwrap();
        let row_enabled = agent_row(&snapshot, "pane_1", &config_enabled, None).unwrap();

        let area = Rect::new(0, 0, 80, 1);
        let mut buf_disabled = Buffer::empty(area);
        render_agent_row(&mut buf_disabled, area, &row_disabled, &config_disabled);

        let mut buf_enabled = Buffer::empty(area);
        render_agent_row(&mut buf_enabled, area, &row_enabled, &config_enabled);

        assert_eq!(
            buf_disabled.content, buf_enabled.content,
            "Normal Herdr agent must render 100% identically with shepherd_enabled true or false"
        );
    }

    #[test]
    fn test_shepherd_enabled_false_no_markers() {
        let config = test_config(false);
        let shepherd_info = crate::api::schema::AgentShepherdInfo {
            id: "coder-1".into(),
            role: "coder".into(),
            state: "blocked".into(),
            notice: Some("urgent attention needed".into()),
        };
        let snapshot = test_snapshot_with_agent(Some(shepherd_info), crate::api::schema::AgentStatus::Blocked);
        let row = agent_row(&snapshot, "pane_1", &config, None).expect("agent row should exist");

        let area = Rect::new(0, 0, 80, 2);
        let mut buffer = Buffer::empty(area);
        render_agent_row(&mut buffer, area, &row, &config);
        let text = buffer_text(&buffer, area);

        assert!(!text.contains("[NEEDS YOU]"), "When shepherd is disabled, must not show [NEEDS YOU]");
        assert!(!text.contains("[blocked]"), "When shepherd is disabled, must not show [blocked]");
        assert!(!text.contains("urgent attention needed"), "When shepherd is disabled, must not show notice");
    }

    #[test]
    fn test_small_dimensions_without_panic() {
        let config = test_config(true);
        let shepherd_info = crate::api::schema::AgentShepherdInfo {
            id: "coder-1".into(),
            role: "coder".into(),
            state: "blocked".into(),
            notice: Some("notice".into()),
        };
        let snapshot = test_snapshot_with_agent(Some(shepherd_info), crate::api::schema::AgentStatus::Blocked);
        let row = agent_row(&snapshot, "pane_1", &config, None).unwrap();

        let test_dimensions = [
            (0, 0),
            (1, 0),
            (0, 1),
            (1, 1),
            (2, 1),
            (5, 1),
            (5, 2),
            (10, 1),
            (10, 2),
        ];

        for (w, h) in test_dimensions {
            let area = Rect::new(0, 0, w, h);
            let mut buffer = Buffer::empty(area);
            render_agent_row(&mut buffer, area, &row, &config);
        }
    }
}
