use super::*;
use ratatui::{
    text::Line,
    widgets::{Paragraph, Widget},
};

fn workspace_selection_background(palette: &Palette) -> ratatui::style::Color {
    if palette.selection_bg == ratatui::style::Color::Reset {
        palette.active_row_bg
    } else {
        palette.selection_bg
    }
}

pub(in crate::client::shell) fn workspace_active_background(
    palette: &Palette,
    navigating: bool,
) -> ratatui::style::Color {
    // The fallback cursor shares the active-row color; only fill the cursor while navigating.
    if navigating && palette.selection_bg == ratatui::style::Color::Reset {
        palette.sidebar_bg
    } else {
        palette.active_row_bg
    }
}

pub(in crate::client::shell) fn collapsed_sidebar_sections(
    area: Rect,
) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.is_empty() {
        return (Rect::default(), None, Rect::default());
    }
    if content.height < 7 {
        return (content, None, Rect::default());
    }
    let workspace_height = content.height.div_ceil(2);
    let divider_y = content.y + workspace_height;
    let detail_height = content.height.saturating_sub(workspace_height + 1);
    (
        Rect::new(content.x, content.y, content.width, workspace_height),
        Some(divider_y),
        Rect::new(content.x, divider_y + 1, content.width, detail_height),
    )
}

pub(crate) fn render_collapsed_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    selected_workspace_id: Option<&str>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    let selection_background = workspace_selection_background(palette);
    let active_background = workspace_active_background(palette, selected_workspace_id.is_some());
    render_sidebar_background(buffer, area, palette);
    let (workspace_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    for (index, workspace) in snapshot
        .workspaces
        .iter()
        .take(workspace_area.height as usize)
        .enumerate()
    {
        let rect = Rect::new(
            workspace_area.x,
            workspace_area.y + index as u16,
            workspace_area.width,
            1,
        );
        let selected = selected_workspace_id == Some(workspace.workspace_id.as_str());
        if selected {
            buffer.set_style(rect, Style::default().bg(selection_background));
        } else if workspace.focused {
            buffer.set_style(rect, Style::default().bg(active_background));
        }
        let number_style = if selected {
            Style::default()
                .fg(palette.overlay1)
                .bg(selection_background)
        } else if workspace.focused {
            Style::default().fg(palette.text).bg(active_background)
        } else {
            Style::default().fg(palette.overlay0)
        };
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            number_style,
        );
        let status = workspace.agent_status;
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            status_icon(status, config.status_indicators),
            Style::default().fg(status_color(status, palette)),
        );
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: workspace.workspace_id.clone(),
            indented: false,
            group_toggle: None,
        });
    }

    if let Some(divider_y) = divider_y {
        put_text(
            buffer,
            workspace_area.x,
            divider_y,
            workspace_area.width,
            &"─".repeat(workspace_area.width as usize),
            Style::default().fg(palette.surface_dim),
        );
    }

    let detail_content = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    for (index, pane_id) in super::ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .take(detail_content.height as usize)
        .enumerate()
    {
        let Some(agent) = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)
        else {
            continue;
        };
        let rect = Rect::new(
            detail_content.x,
            detail_content.y + index as u16,
            detail_content.width,
            1,
        );
        if agent.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            Style::default().fg(if agent.focused {
                palette.text
            } else {
                palette.overlay0
            }),
        );
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            status_icon(agent.agent_status, config.status_indicators),
            Style::default().fg(status_color(agent.agent_status, palette)),
        );
        hits.agents.push((rect, pane_id));
    }
    hits.sidebar_toggle = if area.is_empty() || workspace_area.width == 0 {
        Rect::default()
    } else {
        Rect::new(
            workspace_area.x + workspace_area.width / 2,
            area.bottom().saturating_sub(1),
            1,
            1,
        )
    };
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "»",
        if super::super::global_menu::global_menu_attention(snapshot) {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        },
    );
}

fn widget_tone_color(
    tone: crate::protocol::ClientShellSidebarWidgetTone,
    palette: &Palette,
) -> ratatui::style::Color {
    match tone {
        crate::protocol::ClientShellSidebarWidgetTone::Ok => palette.green,
        crate::protocol::ClientShellSidebarWidgetTone::Warn => palette.yellow,
        crate::protocol::ClientShellSidebarWidgetTone::Err => palette.red,
        crate::protocol::ClientShellSidebarWidgetTone::Accent => palette.accent,
        crate::protocol::ClientShellSidebarWidgetTone::Muted => palette.overlay0,
        crate::protocol::ClientShellSidebarWidgetTone::Normal
        | crate::protocol::ClientShellSidebarWidgetTone::Unknown => palette.text,
    }
}

fn widget_right_style(
    tone: crate::protocol::ClientShellSidebarWidgetTone,
    palette: &Palette,
) -> Style {
    match tone {
        crate::protocol::ClientShellSidebarWidgetTone::Ok => Style::default().fg(palette.green),
        crate::protocol::ClientShellSidebarWidgetTone::Warn => Style::default().fg(palette.yellow),
        crate::protocol::ClientShellSidebarWidgetTone::Err => Style::default().fg(palette.red),
        crate::protocol::ClientShellSidebarWidgetTone::Accent => {
            Style::default().fg(palette.accent)
        }
        crate::protocol::ClientShellSidebarWidgetTone::Normal
        | crate::protocol::ClientShellSidebarWidgetTone::Muted
        | crate::protocol::ClientShellSidebarWidgetTone::Unknown => {
            Style::default().fg(palette.subtext0)
        }
    }
}

fn is_widget_status_glyph(ch: char) -> bool {
    matches!(
        ch,
        '·' | '●' | '✓' | '↑' | '×' | '!' | '○' | '◐' | '◔' | '▸' | '▪'
    )
}

fn render_widget_row_left(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    max_width: u16,
    text: &str,
    tone: crate::protocol::ClientShellSidebarWidgetTone,
    palette: &Palette,
) {
    if max_width == 0 {
        return;
    }
    let truncated = crate::ui::truncate_end(text, usize::from(max_width));
    let base_style = if tone == crate::protocol::ClientShellSidebarWidgetTone::Muted {
        Style::default().fg(palette.overlay0)
    } else {
        Style::default().fg(palette.text)
    };
    let bar_filled_style = Style::default().fg(match tone {
        crate::protocol::ClientShellSidebarWidgetTone::Ok => palette.green,
        crate::protocol::ClientShellSidebarWidgetTone::Warn => palette.yellow,
        crate::protocol::ClientShellSidebarWidgetTone::Err => palette.red,
        _ => palette.accent,
    });
    let bar_empty_style = Style::default()
        .fg(palette.overlay0)
        .add_modifier(Modifier::DIM);
    let glyph_style = Style::default().fg(widget_tone_color(tone, palette));
    let color_leading_glyph = !matches!(
        tone,
        crate::protocol::ClientShellSidebarWidgetTone::Normal
            | crate::protocol::ClientShellSidebarWidgetTone::Muted
            | crate::protocol::ClientShellSidebarWidgetTone::Unknown
    );

    let mut col = x;
    let limit = x.saturating_add(max_width);
    for (index, ch) in truncated.chars().enumerate() {
        let mut buf = [0u8; 4];
        let symbol = ch.encode_utf8(&mut buf);
        let ch_width = display_width(symbol).max(1);
        if col.saturating_add(ch_width) > limit {
            break;
        }
        let style = if index == 0 && color_leading_glyph && is_widget_status_glyph(ch) {
            glyph_style
        } else if ch == '▓' {
            bar_filled_style
        } else if ch == '░' {
            bar_empty_style
        } else {
            base_style
        };
        put_text(buffer, col, y, ch_width, symbol, style);
        col = col.saturating_add(ch_width);
    }
}

enum WidgetBodyRow<'a> {
    Structured(&'a crate::protocol::ClientShellSidebarWidgetRow),
    Plain(&'a str),
}

fn widget_body_rows(widget: &crate::protocol::ClientShellSidebarWidget) -> Vec<WidgetBodyRow<'_>> {
    if !widget.rows.is_empty() {
        widget
            .rows
            .iter()
            .take(crate::sidebar_widgets::MAX_SIDEBAR_WIDGET_ROWS)
            .map(WidgetBodyRow::Structured)
            .collect()
    } else {
        widget
            .lines
            .iter()
            .take(crate::sidebar_widgets::MAX_SIDEBAR_WIDGET_LINES)
            .map(|line| WidgetBodyRow::Plain(line.as_str()))
            .collect()
    }
}

fn widget_height(widget: &crate::protocol::ClientShellSidebarWidget) -> u16 {
    let body_count = widget_body_rows(widget).len() as u16;
    if body_count == 0 {
        1
    } else {
        // Header + 1 blank spacer row + body rows
        2 + body_count
    }
}

fn render_single_widget(
    buffer: &mut Buffer,
    area_x: u16,
    mut y: u16,
    body_limit: u16,
    content_width: u16,
    widget: &crate::protocol::ClientShellSidebarWidget,
    palette: &Palette,
    hits: &mut ShellHitMap,
) -> u16 {
    if y >= body_limit {
        return y;
    }
    let header_style = Style::default()
        .fg(palette.overlay0)
        .add_modifier(Modifier::BOLD);
    let header_right_style = Style::default().fg(palette.subtext0);
    let inner_width = content_width.saturating_sub(1);
    let start_y = y;

    // Header line: left-aligned title, optional right-aligned status text.
    let right_text = widget
        .right
        .as_deref()
        .filter(|text| !text.is_empty())
        .map(|text| format!("{text} "));
    let right_width = right_text
        .as_deref()
        .map(|text| display_width(text).min(content_width.saturating_sub(2)))
        .unwrap_or(0);
    let title_budget =
        inner_width.saturating_sub(right_width.saturating_add(u16::from(right_width > 0)));
    let title = crate::ui::truncate_end(&widget.title, usize::from(title_budget));
    put_text(
        buffer,
        area_x,
        y,
        content_width.saturating_sub(right_width),
        &format!(" {title}"),
        header_style,
    );
    if let Some(right_text) = right_text {
        if right_width > 0 {
            let row_rect = Rect::new(area_x, y, content_width, 1);
            let truncated_right = crate::ui::truncate_end(&right_text, usize::from(right_width));
            put_right_text(buffer, row_rect, y, &truncated_right, header_right_style);
        }
    }
    y = y.saturating_add(1);

    let rows = widget_body_rows(widget);
    if !rows.is_empty() && y < body_limit {
        // 1 blank spacer row between widget header and first row (matches spaces/machines).
        y = y.saturating_add(1);
    }

    for row in rows {
        if y >= body_limit {
            break;
        }
        match row {
            WidgetBodyRow::Plain(line) => {
                let truncated = crate::ui::truncate_end(line, usize::from(inner_width));
                put_text(
                    buffer,
                    area_x,
                    y,
                    content_width,
                    &format!(" {truncated}"),
                    Style::default().fg(palette.text),
                );
            }
            WidgetBodyRow::Structured(row) => {
                let right_text = row
                    .right
                    .as_deref()
                    .filter(|text| !text.is_empty())
                    .map(|text| format!("{text} "));
                let right_width = right_text
                    .as_deref()
                    .map(|text| display_width(text).min(content_width.saturating_sub(2)))
                    .unwrap_or(0);
                let left_budget = inner_width
                    .saturating_sub(right_width.saturating_add(u16::from(right_width > 0)));
                render_widget_row_left(
                    buffer,
                    area_x.saturating_add(1),
                    y,
                    left_budget,
                    &row.left,
                    row.tone,
                    palette,
                );
                if let Some(right_text) = right_text {
                    if right_width > 0 {
                        let row_rect = Rect::new(area_x, y, content_width, 1);
                        let truncated_right =
                            crate::ui::truncate_end(&right_text, usize::from(right_width));
                        put_right_text(
                            buffer,
                            row_rect,
                            y,
                            &truncated_right,
                            widget_right_style(row.tone, palette),
                        );
                    }
                }
            }
        }
        y = y.saturating_add(1);
    }

    if let Some(action) = widget.action.as_ref().filter(|action| !action.is_empty()) {
        let height = y.saturating_sub(start_y);
        if height > 0 {
            hits.sidebar_widgets.push((
                Rect::new(area_x, start_y, content_width, height),
                action.clone(),
            ));
        }
    }

    y
}

pub(in crate::client::shell) fn parse_sidebar_widget_action(
    spec: &str,
) -> Option<crate::api::schema::Method> {
    let mut parts = spec.split(':');
    if parts.next()? != "plugin" {
        return None;
    }
    let plugin_id = parts.next()?.trim();
    let entrypoint = parts.next()?.trim();
    if plugin_id.is_empty() || entrypoint.is_empty() {
        return None;
    }
    let placement = match parts.next().map(str::trim) {
        None | Some("") => None,
        Some("overlay") => Some(crate::api::schema::PluginPanePlacement::Overlay),
        Some("popup") => Some(crate::api::schema::PluginPanePlacement::Popup),
        Some("split") => Some(crate::api::schema::PluginPanePlacement::Split),
        Some("tab") => Some(crate::api::schema::PluginPanePlacement::Tab),
        Some("zoomed") => Some(crate::api::schema::PluginPanePlacement::Zoomed),
        Some(_) => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(crate::api::schema::Method::PluginPaneOpen(
        crate::api::schema::PluginPaneOpenParams {
            plugin_id: plugin_id.to_owned(),
            entrypoint: entrypoint.to_owned(),
            placement,
            width: None,
            height: None,
            workspace_id: None,
            target_pane_id: None,
            direction: None,
            cwd: None,
            focus: true,
            env: std::collections::HashMap::new(),
        },
    ))
}

pub(in crate::client::shell) fn render_top_sidebar_widgets(
    buffer: &mut Buffer,
    area: Rect,
    widgets: &[crate::protocol::ClientShellSidebarWidget],
    palette: &Palette,
    hits: &mut ShellHitMap,
) -> Rect {
    let content_width = area.width.saturating_sub(1);
    if content_width == 0 || area.height < 8 {
        return area;
    }
    let mut top_widgets: Vec<&crate::protocol::ClientShellSidebarWidget> = widgets
        .iter()
        .filter(|widget| widget.slot == crate::protocol::ClientShellSidebarWidgetSlot::Top)
        .collect();
    if top_widgets.is_empty() {
        return area;
    }
    top_widgets.sort_by(|a, b| a.source.cmp(&b.source));

    let max_widget_rows = area.height.saturating_sub(6);
    if max_widget_rows < 2 {
        return area;
    }

    let divider_style = Style::default().fg(palette.surface_dim);

    let mut y = area.y;
    let body_limit = area.y.saturating_add(max_widget_rows.saturating_sub(1));
    let mut rendered_any = false;

    for widget in top_widgets {
        if y >= body_limit {
            break;
        }
        if rendered_any {
            if y.saturating_add(1) >= body_limit {
                break;
            }
            put_text(
                buffer,
                area.x,
                y,
                content_width,
                &"─".repeat(usize::from(content_width)),
                divider_style,
            );
            y = y.saturating_add(1);
        }
        rendered_any = true;
        y = render_single_widget(
            buffer,
            area.x,
            y,
            body_limit,
            content_width,
            widget,
            palette,
            hits,
        );
    }

    if !rendered_any {
        return area;
    }

    put_text(
        buffer,
        area.x,
        y,
        content_width,
        &"─".repeat(usize::from(content_width)),
        divider_style,
    );
    y = y.saturating_add(1);

    let used = y.saturating_sub(area.y);
    Rect::new(area.x, y, area.width, area.height.saturating_sub(used))
}

pub(in crate::client::shell) fn render_bottom_sidebar_widgets(
    buffer: &mut Buffer,
    area: Rect,
    widgets: &[crate::protocol::ClientShellSidebarWidget],
    palette: &Palette,
    show_agents: bool,
    hits: &mut ShellHitMap,
) -> (Rect, bool) {
    let content_width = area.width.saturating_sub(1);
    let min_middle_rows: u16 = if show_agents { 8 } else { 5 };
    let toggle_reserve: u16 = 1;
    if content_width == 0 || area.height < min_middle_rows + toggle_reserve + 2 {
        return (area, false);
    }
    let mut bottom_widgets: Vec<&crate::protocol::ClientShellSidebarWidget> = widgets
        .iter()
        .filter(|widget| widget.slot == crate::protocol::ClientShellSidebarWidgetSlot::Bottom)
        .collect();
    if bottom_widgets.is_empty() {
        return (area, false);
    }
    bottom_widgets.sort_by(|a, b| a.source.cmp(&b.source));

    let max_bottom_rows = area.height.saturating_sub(min_middle_rows + toggle_reserve);
    if max_bottom_rows < 2 {
        return (area, false);
    }

    // Each bottom widget has a leading separator line plus its own height.
    let needed: u16 = bottom_widgets
        .iter()
        .map(|widget| 1u16.saturating_add(widget_height(widget)))
        .fold(0u16, u16::saturating_add);
    let used = needed.min(max_bottom_rows);

    let start_y = area.bottom().saturating_sub(toggle_reserve + used);
    let limit_y = area.bottom().saturating_sub(toggle_reserve);
    let divider_style = Style::default().fg(palette.surface_dim);

    let mut y = start_y;
    for widget in bottom_widgets {
        if y.saturating_add(1) >= limit_y {
            break;
        }
        put_text(
            buffer,
            area.x,
            y,
            content_width,
            &"─".repeat(usize::from(content_width)),
            divider_style,
        );
        y = y.saturating_add(1);
        y = render_single_widget(
            buffer,
            area.x,
            y,
            limit_y,
            content_width,
            widget,
            palette,
            hits,
        );
    }

    let remaining_height = start_y.saturating_sub(area.y);
    (
        Rect::new(area.x, area.y, area.width, remaining_height),
        true,
    )
}

pub(crate) fn render_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    hits.sidebar_divider = if area.is_empty() {
        Rect::default()
    } else {
        Rect::new(area.right().saturating_sub(1), area.y, 1, area.height)
    };
    let sections_area =
        render_top_sidebar_widgets(buffer, area, &snapshot.sidebar_widgets, palette, hits);
    let (middle_area, rendered_bottom_widgets) = render_bottom_sidebar_widgets(
        buffer,
        sections_area,
        &snapshot.sidebar_widgets,
        palette,
        config.show_agents,
        hits,
    );
    let (workspace_area, detail_area) = if config.show_agents {
        hits.sidebar_section_divider =
            crate::ui::sidebar_section_divider_rect(middle_area, state.sidebar_section_split);
        crate::ui::expanded_sidebar_sections(middle_area, state.sidebar_section_split)
    } else {
        hits.sidebar_section_divider = Rect::default();
        let ws_height = if rendered_bottom_widgets {
            middle_area.height
        } else {
            middle_area.height.saturating_sub(1)
        };
        (
            Rect::new(
                middle_area.x,
                middle_area.y,
                middle_area.width.saturating_sub(1),
                ws_height,
            ),
            Rect::default(),
        )
    };
    put_text(
        buffer,
        workspace_area.x,
        workspace_area.y,
        workspace_area.width,
        " spaces",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );

    let entries = workspace_entries(snapshot, state.collapsed_groups);
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(WORKSPACE_HEADER_ROWS),
        workspace_area.width,
        workspace_area
            .height
            .saturating_sub(WORKSPACE_HEADER_ROWS + 1),
    );
    hits.workspace_body = body;
    let row_heights = entries
        .iter()
        .map(|entry| {
            snapshot
                .workspaces
                .get(entry.index)
                .map(|workspace| {
                    workspace_rows(
                        workspace,
                        displayed_workspace_status(snapshot, workspace, state.collapsed_groups),
                        entry.indented,
                        &config.spaces,
                    )
                    .len()
                    .max(1)
                    .min(u16::MAX as usize) as u16
                })
                .unwrap_or(1)
        })
        .collect::<Vec<_>>();
    let gaps = entries
        .iter()
        .enumerate()
        .map(|(index, _)| {
            entries
                .get(index + 1)
                .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap)
        })
        .collect::<Vec<_>>();
    let mut metrics = super::scroll::list_scroll_metrics(
        &row_heights,
        &gaps,
        body.height,
        *state.workspace_scroll,
    );
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = entries
            .iter()
            .position(|entry| snapshot.workspaces[entry.index].focused)
        {
            *state.workspace_scroll = super::scroll::list_scroll_start_to_reveal(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
                target,
            );
            metrics = super::scroll::list_scroll_metrics(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
            );
        }
    }
    hits.workspace_max_scroll = metrics.max_offset_from_bottom;
    hits.workspace_scroll_metrics = Some(metrics);
    *state.workspace_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (entry_position, entry) in entries.iter().enumerate().skip(*state.workspace_scroll) {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let status = displayed_workspace_status(snapshot, workspace, state.collapsed_groups);
        let rows = workspace_rows(workspace, status, entry.indented, &config.spaces);
        let row_height = (rows.len().max(1).min(u16::MAX as usize) as u16).min(body.height);
        if y.saturating_add(row_height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, row_height);
        let selected = state.selected_workspace_id.is_some_and(|target| {
            target.matches(state.active_endpoint_id, &workspace.workspace_id)
        });
        let dragged = state.dragged_workspace_id == Some(workspace.workspace_id.as_str());
        if selected {
            buffer.set_style(rect, Style::default().bg(palette.selection_bg));
        } else if dragged {
            buffer.set_style(rect, Style::default().bg(palette.surface1));
        } else if workspace.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        render_workspace_rows(
            buffer,
            rect,
            status,
            config.status_indicators,
            entry,
            rows,
            workspace.focused,
            selected,
            state.selected_workspace_id.is_some(),
            dragged,
            palette,
        );
        let group_toggle = render_parent_group_toggle(
            buffer,
            rect,
            snapshot,
            entry.index,
            state.collapsed_groups,
            palette,
        );
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: workspace.workspace_id.clone(),
            indented: entry.indented,
            group_toggle,
        });
        let gap = entries
            .get(entry_position + 1)
            .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap);
        y = y.saturating_add(row_height + gap);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.workspace_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    if let Some(row) = state.workspace_drop_indicator_row.filter(|row| {
        *row >= workspace_area.y.saturating_add(1)
            && *row < workspace_area.bottom().saturating_sub(1)
    }) {
        put_text(
            buffer,
            body.x,
            row,
            body.width,
            &"─".repeat(body.width as usize),
            Style::default().fg(palette.accent),
        );
    }

    let footer_y = workspace_area.bottom().saturating_sub(1);
    if config.mouse_capture {
        hits.new_workspace = Rect::new(
            workspace_area.x,
            footer_y,
            5.min(workspace_area.width),
            u16::from(workspace_area.height > 0),
        );
        put_text(
            buffer,
            workspace_area.x,
            footer_y,
            workspace_area.width,
            " new",
            Style::default().fg(palette.overlay0),
        );
        let attention = super::super::global_menu::global_menu_attention(snapshot);
        let launcher_width = if attention { 8 } else { 6 }.min(workspace_area.width);
        hits.global_launcher = Rect::new(
            workspace_area.right().saturating_sub(launcher_width),
            footer_y,
            launcher_width,
            1,
        );
        if attention {
            let start_x = workspace_area.right().saturating_sub(6);
            put_text(
                buffer,
                start_x,
                footer_y,
                2,
                "● ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            put_text(
                buffer,
                start_x.saturating_add(2),
                footer_y,
                4,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        } else {
            put_right_text(
                buffer,
                workspace_area,
                footer_y,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        }
    }

    if config.show_agents {
        super::render_agent_panel(
            buffer,
            detail_area,
            snapshot,
            config,
            state.agent_scroll,
            hits,
        );
    }

    hits.sidebar_toggle = Rect::new(
        area.right().saturating_sub(2),
        area.bottom().saturating_sub(1),
        u16::from(area.width > 1),
        u16::from(area.height > 0),
    );
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "«",
        Style::default().fg(palette.overlay0),
    );
}

pub(crate) fn workspace_entries(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    let mut members = HashMap::<&str, Vec<usize>>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            members.entry(&worktree.key).or_default().push(index);
        }
    }
    let grouped = members
        .iter()
        .filter(|(_, indices)| {
            indices.iter().any(|index| {
                snapshot.workspaces[*index]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            }) && indices.iter().any(|index| {
                snapshot.workspaces[*index]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| !worktree.is_linked_worktree)
            })
        })
        .map(|(key, _)| *key)
        .collect::<HashSet<_>>();
    let mut emitted = HashSet::<&str>::new();
    let mut entries = Vec::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        let Some(worktree) = workspace
            .worktree
            .as_ref()
            .filter(|worktree| grouped.contains(worktree.key.as_str()))
        else {
            entries.push(WorkspaceEntry {
                index,
                indented: false,
                last_child: false,
            });
            continue;
        };
        if !emitted.insert(&worktree.key) {
            continue;
        }
        let Some(group_members) = members.get(worktree.key.as_str()) else {
            continue;
        };
        for parent in group_members.iter().copied().filter(|member| {
            snapshot.workspaces[*member]
                .worktree
                .as_ref()
                .is_some_and(|worktree| !worktree.is_linked_worktree)
        }) {
            entries.push(WorkspaceEntry {
                index: parent,
                indented: false,
                last_child: false,
            });
        }
        if collapsed_groups.contains(&worktree.key) {
            if let Some(active) = group_members.iter().copied().find(|member| {
                let workspace = &snapshot.workspaces[*member];
                workspace.focused
                    && workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree)
            }) {
                entries.push(WorkspaceEntry {
                    index: active,
                    indented: true,
                    last_child: true,
                });
            }
            continue;
        }
        let children = group_members
            .iter()
            .copied()
            .filter(|member| {
                snapshot.workspaces[*member]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            })
            .collect::<Vec<_>>();
        for (child_index, child) in children.iter().enumerate() {
            entries.push(WorkspaceEntry {
                index: *child,
                indented: true,
                last_child: child_index + 1 == children.len(),
            });
        }
    }
    entries
}

fn parent_group_key(snapshot: &ClientShellSnapshot, index: usize) -> Option<String> {
    let workspace = snapshot.workspaces.get(index)?;
    let worktree = workspace.worktree.as_ref()?;
    if worktree.is_linked_worktree {
        return None;
    }
    snapshot
        .workspaces
        .iter()
        .any(|candidate| {
            candidate.worktree.as_ref().is_some_and(|candidate| {
                candidate.key == worktree.key && candidate.is_linked_worktree
            })
        })
        .then(|| worktree.key.clone())
}

pub(in crate::client::shell) fn workspace_close_is_group(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
) -> bool {
    let Some(worktree) = workspace
        .worktree
        .as_ref()
        .filter(|worktree| !worktree.is_linked_worktree)
    else {
        return false;
    };
    let mut has_child = false;
    for member in &snapshot.workspaces {
        if member.workspace_id == workspace.workspace_id {
            continue;
        }
        if let Some(candidate) = member
            .worktree
            .as_ref()
            .filter(|candidate| candidate.key == worktree.key)
        {
            if !candidate.is_linked_worktree {
                return false;
            }
            has_child = true;
        }
    }
    has_child
}

pub(in crate::client::shell) fn render_parent_group_toggle(
    buffer: &mut Buffer,
    workspace_rect: Rect,
    snapshot: &ClientShellSnapshot,
    workspace_index: usize,
    collapsed_groups: &HashSet<String>,
    palette: &Palette,
) -> Option<(Rect, String)> {
    let key = parent_group_key(snapshot, workspace_index)?;
    let toggle = Rect::new(
        workspace_rect.right().saturating_sub(1),
        workspace_rect.y,
        1,
        1,
    );
    put_text(
        buffer,
        toggle.x,
        toggle.y,
        toggle.width,
        if collapsed_groups.contains(&key) {
            "▸"
        } else {
            "▾"
        },
        Style::default().fg(palette.accent),
    );
    Some((toggle, key))
}

pub(in crate::client::shell) fn displayed_workspace_status(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> crate::api::schema::AgentStatus {
    let Some(worktree) = workspace
        .worktree
        .as_ref()
        .filter(|worktree| !worktree.is_linked_worktree)
    else {
        return workspace.agent_status;
    };
    if !collapsed_groups.contains(&worktree.key) {
        return workspace.agent_status;
    }
    snapshot
        .workspaces
        .iter()
        .filter(|candidate| {
            candidate.worktree.as_ref().is_some_and(|member| {
                member.key == worktree.key
                    && (member.is_linked_worktree
                        || candidate.workspace_id == workspace.workspace_id)
            })
        })
        .map(|candidate| candidate.agent_status)
        .max_by_key(|status| status_priority(*status))
        .unwrap_or(workspace.agent_status)
}

pub(in crate::client::shell) fn workspace_rows(
    workspace: &ClientShellWorkspace,
    status: crate::api::schema::AgentStatus,
    indented: bool,
    config: &SpacesSidebarConfig,
) -> Vec<Vec<crate::ui::ResolvedToken>> {
    let label = if indented && !workspace.custom_label {
        workspace
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("worktree/").or(Some(branch)))
            .unwrap_or(&workspace.label)
    } else {
        &workspace.label
    };
    let token_values = workspace.tokens.iter().cloned().collect::<HashMap<_, _>>();
    crate::ui::sidebar_space_rows(
        config,
        crate::ui::SpaceTokenContext {
            workspace: label,
            branch: workspace.branch.as_deref(),
            state_text: status_text(status),
            ahead_behind: workspace.git_ahead_behind,
            tokens: &token_values,
            suppress_git_details: indented,
        },
    )
}

pub(in crate::client::shell) fn render_workspace_rows(
    buffer: &mut Buffer,
    area: Rect,
    status: crate::api::schema::AgentStatus,
    indicators: crate::config::StatusIndicatorStyle,
    entry: &WorkspaceEntry,
    rows: Vec<Vec<crate::ui::ResolvedToken>>,
    focused: bool,
    selected: bool,
    navigating: bool,
    dragged: bool,
    palette: &Palette,
) {
    for (row_index, row) in rows.iter().enumerate() {
        let y = area.y + row_index as u16;
        if y >= area.bottom() {
            break;
        }
        let mut x = area.x;
        if entry.indented {
            let prefix = if row_index == 0 {
                if entry.last_child {
                    "   └─ "
                } else {
                    "   ├─ "
                }
            } else if entry.last_child {
                "        "
            } else {
                "   │    "
            };
            x = put_segment(
                buffer,
                x,
                y,
                area.right(),
                prefix,
                Style::default().fg(palette.overlay0),
            );
        } else if row_index == 0 {
            x = x.saturating_add(1);
        } else {
            x = x.saturating_add(3);
        }
        let highlighted = focused || dragged;
        let workspace_style = Style::default()
            .fg(if highlighted {
                palette.text
            } else {
                palette.subtext0
            })
            .add_modifier(if highlighted {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let secondary_style = Style::default().fg(if focused {
            palette.mauve
        } else {
            palette.overlay0
        });
        let spans = crate::ui::resolved_token_spans(
            row,
            (
                status_icon(status, indicators),
                Style::default().fg(status_color(status, palette)),
            ),
            Style::default().fg(status_color(status, palette)),
            workspace_style,
            secondary_style,
            Style::default().fg(palette.overlay1),
            palette,
            area.right().saturating_sub(2).saturating_sub(x) as usize,
        );
        Paragraph::new(Line::from(spans)).render(
            Rect::new(x, y, area.right().saturating_sub(2).saturating_sub(x), 1),
            buffer,
        );
    }

    let background = if selected {
        Some(workspace_selection_background(palette))
    } else if dragged {
        Some(palette.surface1)
    } else if focused {
        Some(workspace_active_background(palette, navigating))
    } else {
        None
    };
    if let Some(background) = background {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].set_bg(background);
            }
        }
    }
}
