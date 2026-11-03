//! Read-only Shepherd agents TUI panel.
//!
//! Follows Herdr's UI principles:
//! - State (`ShepherdAgentsPanelState`) is pure data separated from runtime/I/O and stored on `AppState`.
//! - Rendering (`render_shepherd_agents_panel` / `render_shepherd_agents_from_app`) is pure:
//!   it takes `&AppState` / `&ShepherdAgentsPanelState` and only draws to a `ratatui::buffer::Buffer`.
//! - Each agent's native Herdr terminal pane remains intact and is the sole surface for viewing
//!   terminal output or interacting with the agent.

// F1 read-only Shepherd agents panel state and pure render functions;
// interactive key/mouse wiring into the client shell is scheduled for F3.
#![allow(dead_code)]

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::app::state::{AppState, Palette};

pub const SHEPHERD_AGENTS_MODAL_SIZE: (u16, u16) = (84, 22);

/// Pure data projection of a single Shepherd registry agent for TUI presentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdAgentRow {
    pub id: String,
    pub role: String,
    pub vendor: String,
    pub model: String,
    pub state: String,
    pub needs_you: bool,
    pub notice: String,
    pub pane: String,
    pub task_text: String,
}

impl ShepherdAgentRow {
    pub fn from_agent(agent: &shepherd_core::Agent) -> Self {
        let notice = agent.notice.trim().to_owned();
        let needs_you = matches!(agent.state.as_str(), "blocked" | "needs-human")
            || !notice.is_empty();
        Self {
            id: agent.id.clone(),
            role: agent.role.clone(),
            vendor: agent.vendor.clone(),
            model: agent.model.clone(),
            state: agent.state.clone(),
            needs_you,
            notice,
            pane: agent.pane.clone(),
            task_text: agent.task_text.clone(),
        }
    }
}

/// Pure presentation state for the read-only Shepherd agents panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdAgentsPanelState {
    pub open: bool,
    pub scroll: u16,
    pub selected: usize,
    pub entries: Vec<ShepherdAgentRow>,
}

impl ShepherdAgentsPanelState {
    pub fn from_registry(registry: &shepherd_core::AgentRegistryFile) -> Self {
        let entries = registry
            .agents
            .iter()
            .map(ShepherdAgentRow::from_agent)
            .collect();
        Self {
            open: false,
            scroll: 0,
            selected: 0,
            entries,
        }
    }

    pub fn refresh_from_registry(&mut self, registry: &shepherd_core::AgentRegistryFile) {
        self.entries = registry
            .agents
            .iter()
            .map(ShepherdAgentRow::from_agent)
            .collect();
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    pub fn needs_you_count(&self) -> usize {
        self.entries.iter().filter(|row| row.needs_you).count()
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn open_panel(&mut self) {
        self.open = true;
    }

    pub fn close_panel(&mut self) {
        self.open = false;
    }

    pub fn select_next(&mut self) {
        if !self.entries.is_empty() {
            self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_agent(&self) -> Option<&ShepherdAgentRow> {
        self.entries.get(self.selected)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdAgentsPanelRender {
    pub area: Rect,
    pub close_button: Rect,
    pub rows: Vec<(Rect, String)>,
    pub scrollbar: Rect,
    pub max_scroll: usize,
}

/// Pure render entrypoint from `&AppState`. Never mutates state or performs I/O.
pub fn render_shepherd_agents_from_app(
    buffer: &mut Buffer,
    area: Rect,
    app: &AppState,
) -> Option<ShepherdAgentsPanelRender> {
    if !app.shepherd_agents_panel.open {
        return None;
    }
    render_shepherd_agents_panel(buffer, area, &app.shepherd_agents_panel, &app.palette)
}

/// Pure render function for the read-only Shepherd agents modal.
pub fn render_shepherd_agents_panel(
    buffer: &mut Buffer,
    area: Rect,
    panel: &ShepherdAgentsPanelState,
    palette: &Palette,
) -> Option<ShepherdAgentsPanelRender> {
    let outer = crate::ui::centered_popup_rect(
        area,
        SHEPHERD_AGENTS_MODAL_SIZE.0,
        SHEPHERD_AGENTS_MODAL_SIZE.1,
    )?;
    let inner = draw_modal_box(buffer, outer, palette)?;
    if inner.height < 6 || inner.width < 20 {
        return Some(ShepherdAgentsPanelRender {
            area: outer,
            ..ShepherdAgentsPanelRender::default()
        });
    }

    let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
    let base = Style::default()
        .fg(palette.text)
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    let title_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );
    let subtitle_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y.saturating_add(1),
        stack.header.width.saturating_sub(2),
        1,
    );

    buffer.set_stringn(
        title_area.x,
        title_area.y,
        "Shepherd Agents",
        usize::from(title_area.width),
        base.fg(palette.text).add_modifier(Modifier::BOLD),
    );

    let needs_you = panel.needs_you_count();
    let subtitle = if needs_you > 0 {
        format!(
            "{} agent(s) · {} needs you · use native herdr pane to interact",
            panel.entries.len(),
            needs_you
        )
    } else {
        format!(
            "{} agent(s) · read-only registry view · native herdr panes intact",
            panel.entries.len()
        )
    };
    let subtitle_style = if needs_you > 0 {
        base.fg(palette.red).add_modifier(Modifier::BOLD)
    } else {
        base.fg(palette.overlay1)
    };
    buffer.set_stringn(
        subtitle_area.x,
        subtitle_area.y,
        &subtitle,
        usize::from(subtitle_area.width),
        subtitle_style,
    );

    let close_rect = crate::ui::release_notes_close_button_rect(Rect::new(
        stack.header.x,
        stack.header.y,
        stack.header.width,
        1,
    ));
    let contrast_fg = match palette.panel_bg {
        ratatui::style::Color::Reset => palette.surface_dim,
        color => color,
    };
    let close_style = Style::default()
        .fg(contrast_fg)
        .bg(palette.accent)
        .add_modifier(Modifier::BOLD)
        .remove_modifier(Modifier::DIM);
    buffer.set_style(close_rect, close_style);
    buffer.set_stringn(
        close_rect.x,
        close_rect.y,
        " esc close ",
        usize::from(close_rect.width),
        close_style,
    );

    let body = stack.content;
    let lines = build_agent_lines(panel, body.width, palette);
    let viewport_rows = usize::from(body.height.max(1));
    let total_rows = lines.len();
    let max_scroll = total_rows.saturating_sub(viewport_rows);
    let scroll = usize::from(panel.scroll).min(max_scroll);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    };
    let track = crate::ui::release_notes_scrollbar_rect(body, metrics);
    let text_area = track
        .map(|_| Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height))
        .unwrap_or(body);

    let mut hit_rows = Vec::new();
    for (visual_idx, (agent_id, line)) in lines
        .into_iter()
        .skip(scroll)
        .take(viewport_rows)
        .enumerate()
    {
        let row_y = text_area.y.saturating_add(visual_idx as u16);
        let row_rect = Rect::new(text_area.x, row_y, text_area.width, 1);
        Paragraph::new(line).render(row_rect, buffer);
        if let Some(id) = agent_id {
            hit_rows.push((row_rect, id));
        }
    }

    if let Some(track) = track {
        crate::ui::render_scrollbar_buffer(
            buffer,
            metrics,
            track,
            palette.overlay0,
            palette.overlay1,
            "▐",
        );
    }

    if let Some(footer_area) = stack.footer {
        let footer = Line::from(vec![
            Span::styled(" scroll ", base.fg(palette.overlay0)),
            Span::styled("wheel ↑↓", base.fg(palette.text)),
            Span::styled("  ·  ", base.fg(palette.overlay0)),
            Span::styled("focus ", base.fg(palette.overlay0)),
            Span::styled("enter", base.fg(palette.text)),
            Span::styled("  ·  ", base.fg(palette.overlay0)),
            Span::styled("close ", base.fg(palette.overlay0)),
            Span::styled("esc", base.fg(palette.text)),
        ]);
        Paragraph::new(footer).render(footer_area, buffer);
    }

    Some(ShepherdAgentsPanelRender {
        area: outer,
        close_button: close_rect,
        rows: hit_rows,
        scrollbar: track.unwrap_or_default(),
        max_scroll,
    })
}

fn build_agent_lines<'a>(
    panel: &'a ShepherdAgentsPanelState,
    width: u16,
    palette: &Palette,
) -> Vec<(Option<String>, Line<'a>)> {
    let base = Style::default()
        .fg(palette.text)
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    if panel.entries.is_empty() {
        return vec![(
            None,
            Line::from(vec![Span::styled(
                " No Shepherd agents registered in this session.",
                base.fg(palette.overlay1),
            )]),
        )];
    }

    let mut out = Vec::with_capacity(panel.entries.len() * 2);
    for (idx, entry) in panel.entries.iter().enumerate() {
        let selected = idx == panel.selected;
        let row_bg = if selected {
            palette.active_row_bg
        } else {
            palette.panel_bg
        };
        let row_base = base.bg(row_bg);
        let (icon, state_color) = match entry.state.as_str() {
            "blocked" | "needs-human" | "failed" => ("●", palette.red),
            "working" | "starting" | "coding" | "reviewing" => ("●", palette.yellow),
            "done" => ("●", palette.teal),
            "idle" => ("○", palette.green),
            _ => ("·", palette.overlay0),
        };
        let vendor_label = if entry.model.is_empty() {
            entry.vendor.clone()
        } else {
            format!("{}/{}", entry.vendor, entry.model)
        };
        let pane_label = if entry.pane.is_empty() {
            String::new()
        } else {
            format!(" [{}]", entry.pane)
        };
        let mut spans = vec![
            Span::styled(" ", row_base),
            Span::styled(
                icon,
                row_base.fg(state_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", row_base),
            Span::styled(
                entry.id.clone(),
                row_base.fg(palette.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  role:", row_base.fg(palette.overlay0)),
            Span::styled(entry.role.clone(), row_base.fg(palette.mauve)),
            Span::styled("  vendor:", row_base.fg(palette.overlay0)),
            Span::styled(vendor_label, row_base.fg(palette.blue)),
            Span::styled("  state:", row_base.fg(palette.overlay0)),
            Span::styled(entry.state.clone(), row_base.fg(state_color)),
        ];
        if !pane_label.is_empty() {
            spans.push(Span::styled(pane_label, row_base.fg(palette.subtext0)));
        }
        if entry.needs_you {
            spans.push(Span::styled("  ", row_base));
            spans.push(Span::styled(
                " NEEDS YOU ",
                Style::default()
                    .fg(match palette.panel_bg {
                        ratatui::style::Color::Reset => palette.surface_dim,
                        color => color,
                    })
                    .bg(palette.red)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        out.push((Some(entry.id.clone()), Line::from(spans)));

        if entry.needs_you && !entry.notice.is_empty() {
            let max_notice = usize::from(width.saturating_sub(6)).max(12);
            let truncated = crate::ui::truncate_end(&entry.notice, max_notice);
            out.push((
                Some(entry.id.clone()),
                Line::from(vec![
                    Span::styled("   ↳ needs you: ", base.fg(palette.red)),
                    Span::styled(truncated, base.fg(palette.text)),
                ]),
            ));
        }
    }
    out
}

fn draw_modal_box(buffer: &mut Buffer, area: Rect, palette: &Palette) -> Option<Rect> {
    if area.width < 2 || area.height < 2 {
        return None;
    }
    let background = Style::default()
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    let border = Style::default()
        .fg(palette.accent)
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            buffer[(x, y)].set_symbol(" ").set_style(background);
        }
    }
    for x in area.x..area.right() {
        buffer[(x, area.y)]
            .set_symbol(if x == area.x {
                "┌"
            } else if x + 1 == area.right() {
                "┐"
            } else {
                "─"
            })
            .set_style(border);
        let bottom_y = area.bottom() - 1;
        buffer[(x, bottom_y)]
            .set_symbol(if x == area.x {
                "└"
            } else if x + 1 == area.right() {
                "┘"
            } else {
                "─"
            })
            .set_style(border);
    }
    for y in area.y + 1..area.bottom() - 1 {
        buffer[(area.x, y)].set_symbol("│").set_style(border);
        buffer[(area.right() - 1, y)]
            .set_symbol("│")
            .set_style(border);
    }
    Some(Rect::new(
        area.x + 1,
        area.y + 1,
        area.width - 2,
        area.height - 2,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_core::{Agent, AgentRegistryFile};

    #[test]
    fn shepherd_agents_panel_state_navigation_and_focus() {
        let mut reg = AgentRegistryFile::default();
        let a1 = reg.create(
            Agent {
                role: "brain".into(),
                vendor: "claude".into(),
                pane: "w1:p1".into(),
                ..Agent::default()
            },
            "tok1".into(),
            "2026-04-20T00:00:00Z",
        );
        let a2 = reg.create(
            Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                pane: "w1:p2".into(),
                ..Agent::default()
            },
            "tok2".into(),
            "2026-04-20T00:00:00Z",
        );

        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        assert!(!panel.open);
        panel.toggle();
        assert!(panel.open);
        assert_eq!(panel.selected, 0);
        assert_eq!(panel.selected_agent().unwrap().id, a1.id);

        panel.select_next();
        assert_eq!(panel.selected, 1);
        assert_eq!(panel.selected_agent().unwrap().id, a2.id);

        panel.select_next(); // stays at last item
        assert_eq!(panel.selected, 1);

        panel.select_prev();
        assert_eq!(panel.selected, 0);

        let mut app_state = AppState::test_new();
        app_state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        assert!(!app_state.shepherd_agents_panel.open);
        app_state.toggle_shepherd_agents_panel();
        assert!(app_state.shepherd_agents_panel.open);

        let root_pane = app_state.workspaces[0].tabs[0].layout.pane_ids()[0];
        app_state.focus_shepherd_agent_pane(0, root_pane);
        assert!(!app_state.shepherd_agents_panel.open);
    }
}
