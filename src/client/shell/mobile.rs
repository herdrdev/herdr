use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};

use super::render::{display_width, put_segment, put_text};
use super::*;

const MOBILE_BUTTON_WIDTH: u16 = 10;

struct MobileItem {
    lines: Vec<Line<'static>>,
    background: Color,
    target: Option<ClientMobileTarget>,
}

impl MobileItem {
    fn section(label: impl Into<String>, palette: &Palette) -> Self {
        Self {
            lines: vec![Line::from(Span::styled(
                format!(" {} ", label.into()),
                Style::default()
                    .fg(palette.overlay1)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ))],
            background: palette.panel_bg,
            target: None,
        }
    }

    fn action(label: &'static str, target: ClientMobileTarget, palette: &Palette) -> Self {
        Self {
            lines: vec![Line::from(Span::styled(
                label,
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ))],
            background: palette.panel_bg,
            target: Some(target),
        }
    }
}

pub(super) fn render_mobile_header(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    if area.is_empty() {
        return;
    }
    let palette = &config.palette;
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    let button_width = MOBILE_BUTTON_WIDTH.min(area.width);
    let button = Rect::new(
        area.right().saturating_sub(button_width),
        area.y,
        button_width,
        area.height,
    );
    hits.mobile_switch = button;
    let status_width = button.x.saturating_sub(area.x).saturating_sub(1);
    let status = Rect::new(area.x, area.y, status_width, area.height);
    render_header_status(buffer, status, snapshot, config);
    render_header_button(buffer, button, snapshot, config);
}

fn render_header_status(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
) {
    if area.is_empty() {
        return;
    }
    let palette = &config.palette;
    let Some(workspace) = snapshot.focused_workspace_id.as_deref().and_then(|id| {
        snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == id)
    }) else {
        put_text(
            buffer,
            area.x,
            area.y,
            area.width,
            " no workspace",
            Style::default().fg(palette.text).bg(palette.panel_bg),
        );
        return;
    };
    let tab_status = compact_tab_status(snapshot, workspace);
    let tab_width = display_width(&tab_status).saturating_add(1).min(area.width);
    let name_width = area.width.saturating_sub(tab_width);
    put_text(
        buffer,
        area.x,
        area.y,
        name_width.min(3),
        &format!(
            " {} ",
            status_icon(workspace.agent_status, config.status_indicators)
        ),
        Style::default()
            .fg(status_color(workspace.agent_status, palette))
            .bg(palette.panel_bg),
    );
    put_text(
        buffer,
        area.x.saturating_add(3),
        area.y,
        name_width.saturating_sub(3),
        &crate::ui::truncate_end(&workspace.label, usize::from(name_width.saturating_sub(4))),
        Style::default()
            .fg(palette.text)
            .bg(palette.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        buffer,
        area.right().saturating_sub(tab_width).saturating_add(1),
        area.y,
        tab_width.saturating_sub(1),
        &tab_status,
        Style::default().fg(palette.overlay1).bg(palette.panel_bg),
    );
    if area.height > 1 {
        render_agent_summary(
            buffer,
            Rect::new(area.x, area.y + 1, area.width, 1),
            snapshot,
            config,
        );
    }
}

fn render_header_button(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
) {
    if area.is_empty() {
        return;
    }
    let palette = &config.palette;
    buffer.set_style(area, Style::default().bg(palette.surface0));
    for y in area.y..area.bottom() {
        put_text(
            buffer,
            area.x,
            y,
            1,
            "│",
            Style::default()
                .fg(palette.surface_dim)
                .bg(palette.surface0),
        );
    }
    let label_y = if area.height > 1 { area.y + 1 } else { area.y };
    let label = "switch";
    let label_width = display_width(label);
    put_text(
        buffer,
        area.x
            .saturating_add(1)
            .saturating_add(area.width.saturating_sub(1 + label_width) / 2),
        label_y,
        area.width.saturating_sub(1),
        label,
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    if snapshot
        .agents
        .iter()
        .any(|agent| agent.agent_status == crate::api::schema::AgentStatus::Blocked)
    {
        put_text(
            buffer,
            area.right().saturating_sub(1),
            area.y,
            1,
            status_icon(
                crate::api::schema::AgentStatus::Blocked,
                config.status_indicators,
            ),
            Style::default().fg(palette.red).bg(palette.surface0),
        );
    }
}

fn compact_tab_status(snapshot: &ClientShellSnapshot, workspace: &ClientShellWorkspace) -> String {
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace.workspace_id)
        .collect::<Vec<_>>();
    let active = tabs
        .iter()
        .position(|tab| tab.tab_id == workspace.active_tab_id)
        .unwrap_or(0);
    let label = tabs
        .get(active)
        .map(|tab| tab.label.as_str())
        .unwrap_or("1");
    if tabs.len() <= 1 {
        format!("tab {label}")
    } else {
        format!("tab {label} · {}/{}", active + 1, tabs.len())
    }
}

fn render_agent_summary(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
) {
    use crate::api::schema::AgentStatus;
    let counts = [
        (AgentStatus::Blocked, "blocked"),
        (AgentStatus::Done, "done"),
        (AgentStatus::Working, "working"),
        (AgentStatus::Idle, "idle"),
    ]
    .map(|(status, label)| {
        (
            status,
            label,
            snapshot
                .agents
                .iter()
                .filter(|agent| agent.agent_status == status)
                .count(),
        )
    });
    let total = counts.iter().map(|(_, _, count)| count).sum::<usize>();
    let pending = counts[..3].iter().map(|(_, _, count)| count).sum::<usize>();
    if total == 0 {
        put_text(
            buffer,
            area.x,
            area.y,
            area.width,
            " no agents",
            Style::default()
                .fg(config.palette.overlay1)
                .bg(config.palette.panel_bg),
        );
        return;
    }
    if pending == 0 {
        put_text(
            buffer,
            area.x,
            area.y,
            area.width,
            " all idle",
            Style::default()
                .fg(config.palette.overlay1)
                .bg(config.palette.panel_bg),
        );
        return;
    }

    let mut x = area.x.saturating_add(1);
    let mut shown = 0usize;
    let mut omitted = false;
    for (status, label, count) in counts {
        if count == 0 {
            continue;
        }
        let symbol = match (config.status_indicators, status) {
            (crate::config::StatusIndicatorStyle::Dots, AgentStatus::Blocked) => Some("◉"),
            (crate::config::StatusIndicatorStyle::Dots, AgentStatus::Done) => Some("●"),
            (crate::config::StatusIndicatorStyle::Dots, _) => None,
            _ => Some(status_icon(status, config.status_indicators)),
        };
        let text = symbol.map_or_else(
            || format!("{count} {label}"),
            |symbol| format!("{symbol} {count} {label}"),
        );
        let separator = if shown == 0 { "" } else { " · " };
        let needed = display_width(separator).saturating_add(display_width(&text));
        if area.right().saturating_sub(x) < needed {
            omitted = true;
            break;
        }
        if !separator.is_empty() {
            x = put_segment(
                buffer,
                x,
                area.y,
                area.right(),
                separator,
                Style::default()
                    .fg(config.palette.overlay0)
                    .bg(config.palette.panel_bg),
            );
        }
        let color = if shown == 0 {
            match status {
                AgentStatus::Done => config.palette.blue,
                _ => status_color(status, &config.palette),
            }
        } else {
            config.palette.overlay1
        };
        x = put_segment(
            buffer,
            x,
            area.y,
            area.right(),
            &text,
            Style::default()
                .fg(color)
                .bg(config.palette.panel_bg)
                .add_modifier(if shown == 0 {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        );
        shown += 1;
    }
    if omitted && area.right().saturating_sub(x) >= 2 {
        put_text(
            buffer,
            x,
            area.y,
            2,
            " …",
            Style::default()
                .fg(config.palette.overlay0)
                .bg(config.palette.panel_bg),
        );
    }
}

pub(super) fn render_mobile_switcher(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    selected_workspace_id: Option<&str>,
    scroll: &mut usize,
    reveal_workspace: &mut bool,
    hits: &mut ShellHitMap,
) {
    if area.is_empty() {
        return;
    }
    let palette = &config.palette;
    Clear.render(area, buffer);
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    hits.mobile_switch = Rect::default();
    if area.height <= 2 {
        *scroll = 0;
        put_text(
            buffer,
            area.x,
            area.y,
            area.width,
            &"─".repeat(usize::from(area.width)),
            Style::default()
                .fg(palette.surface_dim)
                .bg(palette.panel_bg),
        );
        return;
    }
    let header_height = area.height.min(2);
    let close_width = MOBILE_BUTTON_WIDTH.min(area.width);
    let close = Rect::new(
        area.right().saturating_sub(close_width),
        area.y,
        close_width,
        header_height,
    );
    hits.mobile_close = close;
    put_text(
        buffer,
        area.x,
        area.y,
        close.x.saturating_sub(area.x),
        " switch",
        Style::default()
            .fg(palette.text)
            .bg(palette.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    render_close_button(buffer, close, palette);
    let rule_y = area.y + header_height;
    put_text(
        buffer,
        area.x,
        rule_y,
        area.width,
        &"─".repeat(usize::from(area.width)),
        Style::default()
            .fg(palette.surface_dim)
            .bg(palette.panel_bg),
    );
    let viewport = Rect::new(
        area.x,
        rule_y.saturating_add(1),
        area.width,
        area.height.saturating_sub(header_height + 1),
    );
    if viewport.is_empty() {
        *scroll = 0;
        return;
    }

    let items = mobile_items(
        snapshot,
        config,
        selected_workspace_id,
        viewport.width.saturating_sub(1),
    );
    let total_rows = items.iter().map(|item| item.lines.len()).sum::<usize>();
    let max_scroll = total_rows.saturating_sub(usize::from(viewport.height));
    *scroll = (*scroll).min(max_scroll);
    if *reveal_workspace {
        if let Some(selected_workspace_id) = selected_workspace_id {
            let mut start = 0usize;
            for item in &items {
                let end = start.saturating_add(item.lines.len());
                if matches!(
                    item.target.as_ref(),
                    Some(ClientMobileTarget::Workspace(workspace_id))
                        if workspace_id == selected_workspace_id
                ) {
                    if start < *scroll {
                        *scroll = start;
                    } else if end > (*scroll).saturating_add(usize::from(viewport.height)) {
                        *scroll = end
                            .saturating_sub(usize::from(viewport.height))
                            .min(max_scroll);
                    }
                    break;
                }
                start = end;
            }
        }
        *reveal_workspace = false;
    }
    hits.mobile_max_scroll = max_scroll;
    let content = if viewport.width > 1 {
        Rect::new(
            viewport.x + 1,
            viewport.y,
            viewport.width - 1,
            viewport.height,
        )
    } else {
        Rect::default()
    };
    if max_scroll > 0 {
        render_left_scrollbar(buffer, viewport, total_rows, *scroll, palette);
    }
    if content.is_empty() {
        return;
    }

    let viewport_start = *scroll;
    let viewport_end = viewport_start.saturating_add(usize::from(viewport.height));
    let mut document_row = 0usize;
    for item in items {
        let item_start = document_row;
        let item_end = item_start.saturating_add(item.lines.len());
        let visible_start = item_start.max(viewport_start);
        let visible_end = item_end.min(viewport_end);
        if visible_start < visible_end {
            let y = viewport.y + u16::try_from(visible_start - viewport_start).unwrap_or(u16::MAX);
            let height = u16::try_from(visible_end - visible_start).unwrap_or(u16::MAX);
            let rect = Rect::new(content.x, y, content.width, height);
            buffer.set_style(rect, Style::default().bg(item.background));
            for row in visible_start..visible_end {
                let line = item.lines[row - item_start].clone();
                Paragraph::new(line).render(
                    Rect::new(
                        content.x,
                        viewport.y + u16::try_from(row - viewport_start).unwrap_or(u16::MAX),
                        content.width,
                        1,
                    ),
                    buffer,
                );
            }
            if let Some(target) = item.target {
                hits.mobile_targets.push((rect, target));
            }
        }
        document_row = item_end;
    }
}

fn render_close_button(buffer: &mut Buffer, area: Rect, palette: &Palette) {
    if area.is_empty() {
        return;
    }
    buffer.set_style(area, Style::default().bg(palette.surface0));
    for y in area.y..area.bottom() {
        put_text(
            buffer,
            area.x,
            y,
            1,
            "│",
            Style::default()
                .fg(palette.surface_dim)
                .bg(palette.surface0),
        );
    }
    let label_width = 5;
    let label_x = area
        .x
        .saturating_add(1)
        .saturating_add(area.width.saturating_sub(1 + label_width) / 2);
    put_text(
        buffer,
        label_x,
        area.y,
        area.width.saturating_sub(1),
        "close",
        Style::default()
            .fg(palette.overlay1)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    if area.height > 1 {
        put_text(
            buffer,
            area.x.saturating_add(area.width / 2),
            area.y + 1,
            1,
            "×",
            Style::default()
                .fg(palette.text)
                .bg(palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
    }
}

fn mobile_items(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    selected_workspace_id: Option<&str>,
    content_width: u16,
) -> Vec<MobileItem> {
    let palette = &config.palette;
    let mut items = Vec::new();
    let ordered_agents =
        super::agent_sidebar::ordered_agent_pane_ids(snapshot, config.agent_panel_sort);
    if !ordered_agents.is_empty() || snapshot.agent_view_label.is_some() {
        let title = snapshot
            .agent_view_label
            .as_deref()
            .map(|label| format!("agents · {label}"))
            .unwrap_or_else(|| "agents".to_owned());
        items.push(MobileItem::section(title, palette));
        if ordered_agents.is_empty() {
            items.push(MobileItem {
                lines: vec![Line::from(Span::styled(
                    "  no matching agents",
                    Style::default()
                        .fg(palette.overlay0)
                        .bg(palette.panel_bg)
                        .add_modifier(Modifier::DIM),
                ))],
                background: palette.panel_bg,
                target: None,
            });
        }
        for pane_id in ordered_agents {
            let Some(agent) = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
            else {
                continue;
            };
            let workspace = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == agent.workspace_id);
            let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
            let agent_label = agent
                .display_agent
                .as_deref()
                .or(agent.name.as_deref())
                .or(agent.agent.as_deref())
                .unwrap_or("agent");
            let primary = workspace
                .map(|workspace| workspace.label.as_str())
                .unwrap_or(agent_label);
            let mut detail = Vec::new();
            let workspace_tab_count = snapshot
                .tabs
                .iter()
                .filter(|candidate| candidate.workspace_id == agent.workspace_id)
                .count();
            if let Some(tab) = tab.filter(|tab| tab.custom_label || workspace_tab_count > 1) {
                detail.push(tab.label.clone());
            }
            let status_key = status_text(agent.agent_status);
            detail.push(
                agent
                    .state_labels
                    .iter()
                    .find(|(key, _)| key == status_key)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| {
                        if agent.agent_status == crate::api::schema::AgentStatus::Unknown {
                            "idle".to_owned()
                        } else {
                            status_key.to_owned()
                        }
                    }),
            );
            detail.push(agent_label.to_owned());
            let background = if agent.focused {
                palette.surface_dim
            } else {
                palette.panel_bg
            };
            items.push(MobileItem {
                lines: vec![
                    Line::from(vec![
                        Span::styled("  ", Style::default().bg(background)),
                        Span::styled(
                            status_icon(agent.agent_status, config.status_indicators),
                            Style::default()
                                .fg(status_color(agent.agent_status, palette))
                                .bg(background),
                        ),
                        Span::styled(" ", Style::default().bg(background)),
                        Span::styled(
                            crate::ui::truncate_end(
                                primary,
                                usize::from(content_width.saturating_sub(5)),
                            ),
                            Style::default()
                                .fg(palette.text)
                                .bg(background)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(Span::styled(
                        crate::ui::truncate_end(
                            &format!("  {}", detail.join(" · ")),
                            usize::from(content_width),
                        ),
                        Style::default().fg(palette.overlay0).bg(background),
                    )),
                ],
                background,
                target: Some(ClientMobileTarget::Agent(agent.pane_id.clone())),
            });
        }
    }

    items.push(MobileItem::section("spaces", palette));
    items.push(MobileItem::action(
        "  + new workspace",
        ClientMobileTarget::NewWorkspace,
        palette,
    ));
    for entry in super::render::workspace_entries(snapshot, &HashSet::new()) {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let selected = selected_workspace_id == Some(workspace.workspace_id.as_str());
        let background = if selected {
            palette.surface0
        } else if workspace.focused {
            palette.surface_dim
        } else {
            palette.panel_bg
        };
        let connector = if entry.indented {
            if entry.last_child {
                "└─ "
            } else {
                "├─ "
            }
        } else {
            ""
        };
        let name = if entry.indented && !workspace.custom_label {
            workspace
                .branch
                .as_deref()
                .and_then(|branch| branch.strip_prefix("worktree/").or(Some(branch)))
                .unwrap_or(&workspace.label)
        } else {
            &workspace.label
        };
        let branch = workspace.branch.as_deref().unwrap_or("shell");
        let detail_prefix = if entry.indented {
            if entry.last_child {
                "       "
            } else {
                "  │    "
            }
        } else {
            "  "
        };
        items.push(MobileItem {
            lines: vec![
                Line::from(vec![
                    Span::styled(
                        format!("  {connector}"),
                        Style::default().fg(palette.overlay0).bg(background),
                    ),
                    Span::styled(
                        status_icon(workspace.agent_status, config.status_indicators),
                        Style::default()
                            .fg(status_color(workspace.agent_status, palette))
                            .bg(background),
                    ),
                    Span::styled(" ", Style::default().bg(background)),
                    Span::styled(
                        crate::ui::truncate_end(
                            name,
                            usize::from(content_width.saturating_sub(if entry.indented {
                                8
                            } else {
                                5
                            })),
                        ),
                        Style::default()
                            .fg(palette.text)
                            .bg(background)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    crate::ui::truncate_end(
                        &format!(
                            "{detail_prefix}{branch} · {}",
                            compact_tab_status(snapshot, workspace)
                        ),
                        usize::from(content_width),
                    ),
                    Style::default().fg(palette.overlay0).bg(background),
                )),
            ],
            background,
            target: Some(ClientMobileTarget::Workspace(
                workspace.workspace_id.clone(),
            )),
        });
    }

    if let Some(workspace_id) = snapshot.focused_workspace_id.as_deref() {
        items.push(MobileItem::section("tabs", palette));
        items.push(MobileItem::action(
            "  + new tab",
            ClientMobileTarget::NewTab,
            palette,
        ));
        for (index, tab) in snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace_id)
            .enumerate()
        {
            let background = if tab.focused {
                palette.surface_dim
            } else {
                palette.panel_bg
            };
            let label = if tab.custom_label {
                format!("{} · {}", index + 1, tab.label)
            } else {
                format!("tab {}", tab.label)
            };
            let label = format!(
                "  {}",
                crate::ui::truncate_end(&label, usize::from(content_width.saturating_sub(3)),)
            );
            items.push(MobileItem {
                lines: vec![Line::from(Span::styled(
                    label,
                    Style::default()
                        .fg(palette.text)
                        .bg(background)
                        .add_modifier(Modifier::BOLD),
                ))],
                background,
                target: Some(ClientMobileTarget::Tab(tab.tab_id.clone())),
            });
        }
    }

    items.push(MobileItem::section("menu", palette));
    for (index, (label, _)) in super::global_menu::global_menu_items(snapshot)
        .into_iter()
        .enumerate()
    {
        items.push(MobileItem {
            lines: vec![Line::from(Span::styled(
                format!("  {label}"),
                Style::default().fg(palette.overlay1).bg(palette.panel_bg),
            ))],
            background: palette.panel_bg,
            target: Some(ClientMobileTarget::Menu(index)),
        });
    }
    items
}

impl ClientShellState {
    pub(super) fn handle_mobile_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let mobile = self
            .last_composed_size
            .is_some_and(|(cols, rows)| !self.layout(cols, rows).mobile_header.is_empty());
        if !mobile || self.overlay.is_some() {
            return false;
        }
        use crossterm::event::{MouseButton, MouseEventKind};
        let point = (mouse.column, mouse.row);
        if self.mode != ClientShellMode::Navigate {
            if matches!(
                self.mode,
                ClientShellMode::Terminal | ClientShellMode::Resize
            ) && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && super::contains(self.hits.mobile_switch, point)
            {
                self.mobile_switcher_scroll = 0;
                self.reveal_mobile_workspace = false;
                self.mode = ClientShellMode::Navigate;
                self.navigate_workspace_id = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.focused_workspace_id.clone());
                outcome.repaint = true;
                return true;
            }
            return false;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.mobile_switcher_scroll = self.mobile_switcher_scroll.saturating_sub(2);
                outcome.repaint = true;
                return true;
            }
            MouseEventKind::ScrollDown => {
                self.mobile_switcher_scroll = self
                    .mobile_switcher_scroll
                    .saturating_add(2)
                    .min(self.hits.mobile_max_scroll);
                outcome.repaint = true;
                return true;
            }
            MouseEventKind::Down(MouseButton::Left) => {}
            _ => return true,
        }

        if super::contains(self.hits.mobile_close, point) {
            self.mode = ClientShellMode::Terminal;
            self.navigate_workspace_id = None;
            outcome.repaint = true;
            return true;
        }
        let target = self
            .hits
            .mobile_targets
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, target)| target.clone());
        match target {
            Some(ClientMobileTarget::NewWorkspace) => {
                self.mobile_switcher_suspended = true;
                self.record_binding(
                    crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorkspace),
                    outcome,
                );
            }
            Some(
                target @ (ClientMobileTarget::Workspace(_)
                | ClientMobileTarget::Tab(_)
                | ClientMobileTarget::Agent(_)),
            ) => {
                let method = match target {
                    ClientMobileTarget::Workspace(workspace_id) => {
                        crate::api::schema::Method::WorkspaceFocus(
                            crate::api::schema::WorkspaceTarget { workspace_id },
                        )
                    }
                    ClientMobileTarget::Tab(tab_id) => {
                        crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                            tab_id,
                        })
                    }
                    ClientMobileTarget::Agent(pane_id) => {
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        })
                    }
                    ClientMobileTarget::NewWorkspace
                    | ClientMobileTarget::NewTab
                    | ClientMobileTarget::Menu(_) => return true,
                };
                self.mode = ClientShellMode::Terminal;
                self.navigate_workspace_id = None;
                self.push_endpoint_method(method, outcome);
            }
            Some(ClientMobileTarget::NewTab) => {
                self.mobile_switcher_suspended = true;
                self.record_binding(
                    crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
                    outcome,
                );
            }
            Some(ClientMobileTarget::Menu(index)) => {
                let actionable = self.snapshot.as_deref().is_some_and(|snapshot| {
                    super::global_menu::global_menu_items(snapshot)
                        .get(index)
                        .is_some_and(|(_, action)| {
                            *action != super::global_menu::ClientGlobalMenuAction::WhatsNew
                                || snapshot.release_notes.is_some()
                        })
                });
                if actionable {
                    self.mobile_switcher_suspended = true;
                    self.activate_global_menu_item(index, outcome);
                }
            }
            None => {}
        }
        outcome.repaint = true;
        true
    }
}

fn render_left_scrollbar(
    buffer: &mut Buffer,
    viewport: Rect,
    total_rows: usize,
    scroll: usize,
    palette: &Palette,
) {
    if viewport.is_empty() {
        return;
    }
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: total_rows
            .saturating_sub(usize::from(viewport.height))
            .saturating_sub(scroll),
        max_offset_from_bottom: total_rows.saturating_sub(usize::from(viewport.height)),
        viewport_rows: usize::from(viewport.height),
    };
    let track = Rect::new(viewport.x, viewport.y, 1, viewport.height);
    for y in track.y..track.bottom() {
        put_text(
            buffer,
            track.x,
            y,
            1,
            "│",
            Style::default()
                .fg(palette.surface_dim)
                .bg(palette.panel_bg),
        );
    }
    if let Some(thumb) = crate::ui::scrollbar_thumb(metrics, track) {
        for y in thumb.top..thumb.top.saturating_add(thumb.len) {
            put_text(
                buffer,
                track.x,
                y,
                1,
                "▌",
                Style::default().fg(palette.accent).bg(palette.panel_bg),
            );
        }
    }
}
