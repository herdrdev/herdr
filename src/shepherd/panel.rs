//! Interactive Shepherd agents TUI panel.
//!
//! Follows Shepherd's UI principles:
//! - State (`ShepherdAgentsPanelState`) is pure data separated from runtime/I/O and stored on `AppState`.
//! - Rendering (`render_shepherd_agents_panel` / `render_shepherd_agents_from_app`) is pure:
//!   it takes `&AppState` / `&ShepherdAgentsPanelState` and only draws to a `ratatui::buffer::Buffer`.
//! - Each agent's native Shepherd terminal pane remains intact and is the sole surface for viewing
//!   terminal output or interacting with the agent.
//! - Mouse and keyboard interactions are supported. Every destructive or publishing action
//!   requires explicit confirmation before execution.

#![allow(dead_code)]

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::app::state::{AppState, Palette};

pub const SHEPHERD_AGENTS_MODAL_SIZE: (u16, u16) = (86, 24);

/// High-level actions emitted by the interactive Shepherd agents panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdAgentsAction {
    FocusPane {
        pane_id: String,
        agent_id: String,
    },
    Spawn {
        role: String,
        task: String,
        vendor: Option<String>,
        model: Option<String>,
        extra_skills: Vec<String>,
    },
    Kill {
        agent_id: String,
    },
    Clean {
        agent_id: String,
    },
    CleanAllFinished {
        count: usize,
        workspace: Option<String>,
    },
}

/// Sub-modals displayed within the Shepherd agents panel.
/// Only one sub-modal or confirmation may be active at any given time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentsSubModal {
    None,
    Spawn(SpawnFormState),
    ConfirmKill {
        agent_id: String,
        state: String,
        message: String,
    },
    ConfirmClean {
        agent_id: String,
        state: String,
        message: String,
    },
    ConfirmCleanAll {
        count: usize,
        workspace: Option<String>,
        message: String,
    },
}

impl Default for AgentsSubModal {
    fn default() -> Self {
        Self::None
    }
}

/// Form state for spawning a new agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnFormState {
    pub role: String,
    pub task: String,
    pub vendor: String,
    pub model: String,
    pub extra_skills: String,
    pub active_field: usize, // 0: role, 1: task, 2: vendor, 3: model, 4: extra_skills
    pub error: Option<String>,
}

impl SpawnFormState {
    pub fn validate(&self) -> Result<(), String> {
        if self.role.trim().is_empty() {
            return Err("El rol es obligatorio".into());
        }
        if self.task.trim().is_empty() {
            return Err("La descripción de la tarea es obligatoria".into());
        }
        Ok(())
    }

    pub fn cycle_field_forward(&mut self) {
        self.active_field = (self.active_field + 1) % 5;
    }

    pub fn cycle_field_backward(&mut self) {
        self.active_field = if self.active_field == 0 { 4 } else { self.active_field - 1 };
    }

    pub fn insert_char(&mut self, c: char) {
        match self.active_field {
            0 => self.role.push(c),
            1 => self.task.push(c),
            2 => self.vendor.push(c),
            3 => self.model.push(c),
            4 => self.extra_skills.push(c),
            _ => {}
        }
    }

    pub fn delete_char(&mut self) {
        match self.active_field {
            0 => { self.role.pop(); }
            1 => { self.task.pop(); }
            2 => { self.vendor.pop(); }
            3 => { self.model.pop(); }
            4 => { self.extra_skills.pop(); }
            _ => {}
        }
    }
}

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

/// Pure presentation state for the interactive Shepherd agents panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdAgentsPanelState {
    pub open: bool,
    pub scroll: u16,
    pub selected: usize,
    pub entries: Vec<ShepherdAgentRow>,
    pub sub_modal: AgentsSubModal,
    pub last_action: Option<ShepherdAgentsAction>,
    pub error: Option<String>,
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
            sub_modal: AgentsSubModal::None,
            last_action: None,
            error: None,
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

    pub fn definitive_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|a| matches!(a.state.as_str(), "exited" | "killed" | "failed"))
            .count()
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if !self.open {
            self.sub_modal = AgentsSubModal::None;
        }
    }

    pub fn open_panel(&mut self) {
        self.open = true;
    }

    pub fn close_panel(&mut self) {
        self.open = false;
        self.sub_modal = AgentsSubModal::None;
    }

    pub fn select_next(&mut self) {
        if !self.entries.is_empty() {
            self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_index(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selected = index;
        }
    }

    pub fn selected_agent(&self) -> Option<&ShepherdAgentRow> {
        self.entries.get(self.selected)
    }

    pub fn focus_selected_agent(&mut self) -> Option<ShepherdAgentsAction> {
        let agent = self.selected_agent()?;
        let action = ShepherdAgentsAction::FocusPane {
            pane_id: agent.pane.clone(),
            agent_id: agent.id.clone(),
        };
        self.last_action = Some(action.clone());
        self.close_panel();
        Some(action)
    }

    pub fn open_spawn_form(&mut self) {
        if matches!(self.sub_modal, AgentsSubModal::None) {
            self.sub_modal = AgentsSubModal::Spawn(SpawnFormState::default());
        }
    }

    pub fn open_kill_confirm(&mut self) {
        if !matches!(self.sub_modal, AgentsSubModal::None) {
            return;
        }
        if let Some(agent) = self.selected_agent() {
            let id = agent.id.clone();
            let state = agent.state.clone();
            let message = if state == "done" {
                format!("¿Matar al agente '{id}'? El agente está 'done'; cerrará su pestaña.")
            } else {
                format!("¿Matar al agente '{id}'? Estado: {state}. Se terminará su proceso y se marcará como killed.")
            };
            self.sub_modal = AgentsSubModal::ConfirmKill {
                agent_id: id,
                state,
                message,
            };
        }
    }

    pub fn open_clean_confirm(&mut self) {
        if !matches!(self.sub_modal, AgentsSubModal::None) {
            return;
        }
        if let Some(agent) = self.selected_agent() {
            let id = agent.id.clone();
            let state = agent.state.clone();
            let message = if state == "done" {
                format!("¿Quitar al agente '{id}' de la lista? Estado: done; cerrará su pestaña y eliminará su sesión.")
            } else {
                format!("¿Quitar al agente '{id}' de la lista? Estado: {state}. Se eliminará su registro de sesión y worktree.")
            };
            self.sub_modal = AgentsSubModal::ConfirmClean {
                agent_id: id,
                state,
                message,
            };
        }
    }

    pub fn open_clean_all_confirm(&mut self, workspace: Option<String>) {
        if !matches!(self.sub_modal, AgentsSubModal::None) {
            return;
        }
        let count = self.definitive_count();
        let message = format!(
            "¿Limpiar los {count} agentes definitivos del workspace (exited, killed, failed)? Se cerrarán sus paneles residuales."
        );
        self.sub_modal = AgentsSubModal::ConfirmCleanAll {
            count,
            workspace,
            message,
        };
    }

    pub fn cancel_sub_modal(&mut self) {
        self.sub_modal = AgentsSubModal::None;
    }

    pub fn confirm_active_action(&mut self) -> Option<ShepherdAgentsAction> {
        match std::mem::replace(&mut self.sub_modal, AgentsSubModal::None) {
            AgentsSubModal::None => None,
            AgentsSubModal::Spawn(form) => {
                if let Err(err) = form.validate() {
                    let mut form = form;
                    form.error = Some(err);
                    self.sub_modal = AgentsSubModal::Spawn(form);
                    None
                } else {
                    let vendor = if form.vendor.trim().is_empty() {
                        None
                    } else {
                        Some(form.vendor.trim().to_string())
                    };
                    let model = if form.model.trim().is_empty() {
                        None
                    } else {
                        Some(form.model.trim().to_string())
                    };
                    let extra_skills = form
                        .extra_skills
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    let action = ShepherdAgentsAction::Spawn {
                        role: form.role.trim().to_string(),
                        task: form.task.trim().to_string(),
                        vendor,
                        model,
                        extra_skills,
                    };
                    self.last_action = Some(action.clone());
                    Some(action)
                }
            }
            AgentsSubModal::ConfirmKill { agent_id, .. } => {
                let action = ShepherdAgentsAction::Kill { agent_id };
                self.last_action = Some(action.clone());
                Some(action)
            }
            AgentsSubModal::ConfirmClean { agent_id, .. } => {
                let action = ShepherdAgentsAction::Clean { agent_id };
                self.last_action = Some(action.clone());
                Some(action)
            }
            AgentsSubModal::ConfirmCleanAll { count, workspace, .. } => {
                let action = ShepherdAgentsAction::CleanAllFinished { count, workspace };
                self.last_action = Some(action.clone());
                Some(action)
            }
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<ShepherdAgentsAction> {
        use crossterm::event::KeyCode;
        match self.sub_modal {
            AgentsSubModal::None => match key.code {
                KeyCode::Esc => {
                    self.close_panel();
                    None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.select_prev();
                    None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.select_next();
                    None
                }
                KeyCode::Enter => self.focus_selected_agent(),
                KeyCode::Char('s') => {
                    self.open_spawn_form();
                    None
                }
                KeyCode::Char('K') => {
                    self.open_kill_confirm();
                    None
                }
                KeyCode::Char('x') => {
                    self.open_clean_confirm();
                    None
                }
                KeyCode::Char('X') => {
                    self.open_clean_all_confirm(None);
                    None
                }
                _ => None,
            },
            AgentsSubModal::Spawn(ref mut form) => match key.code {
                KeyCode::Esc => {
                    self.cancel_sub_modal();
                    None
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.cycle_field_forward();
                    None
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.cycle_field_backward();
                    None
                }
                KeyCode::Enter => self.confirm_active_action(),
                KeyCode::Backspace => {
                    form.delete_char();
                    None
                }
                KeyCode::Char(c) => {
                    form.insert_char(c);
                    None
                }
                _ => None,
            },
            AgentsSubModal::ConfirmKill { .. }
            | AgentsSubModal::ConfirmClean { .. }
            | AgentsSubModal::ConfirmCleanAll { .. } => match key.code {
                KeyCode::Esc => {
                    self.cancel_sub_modal();
                    None
                }
                KeyCode::Enter => self.confirm_active_action(),
                _ => None,
            },
        }
    }

    pub fn handle_click(
        &mut self,
        col: u16,
        row: u16,
        render: &ShepherdAgentsPanelRender,
    ) -> Option<ShepherdAgentsAction> {
        if contains_point(render.close_button, col, row) {
            self.close_panel();
            return None;
        }

        match self.sub_modal {
            AgentsSubModal::None => {
                if contains_point(render.spawn_button, col, row) {
                    self.open_spawn_form();
                    return None;
                }
                if contains_point(render.kill_button, col, row) {
                    self.open_kill_confirm();
                    return None;
                }
                if contains_point(render.clean_button, col, row) {
                    self.open_clean_confirm();
                    return None;
                }
                if contains_point(render.clean_all_button, col, row) {
                    self.open_clean_all_confirm(None);
                    return None;
                }
                for (idx, (rect, _agent_id)) in render.rows.iter().enumerate() {
                    if contains_point(*rect, col, row) {
                        if self.selected == idx {
                            // Click on already-selected row or double-click focuses agent
                            return self.focus_selected_agent();
                        } else {
                            self.selected = idx;
                            return None;
                        }
                    }
                }
                None
            }
            _ => {
                if contains_point(render.confirm_button, col, row) {
                    self.confirm_active_action()
                } else if contains_point(render.cancel_button, col, row) {
                    self.cancel_sub_modal();
                    None
                } else if let AgentsSubModal::Spawn(ref mut form) = self.sub_modal {
                    for (field_idx, field_rect) in render.form_fields.iter().enumerate() {
                        if contains_point(*field_rect, col, row) {
                            form.active_field = field_idx;
                            break;
                        }
                    }
                    None
                } else {
                    None
                }
            }
        }
    }
}

pub(crate) fn contains_point(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdAgentsPanelRender {
    pub area: Rect,
    pub close_button: Rect,
    pub rows: Vec<(Rect, String)>,
    pub scrollbar: Rect,
    pub max_scroll: usize,
    pub confirm_button: Rect,
    pub cancel_button: Rect,
    pub spawn_button: Rect,
    pub kill_button: Rect,
    pub clean_button: Rect,
    pub clean_all_button: Rect,
    pub form_fields: Vec<Rect>,
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

/// Pure render function for the interactive Shepherd agents modal.
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
    let inner = draw_modal_box(buffer, outer, palette.panel_bg, palette.accent)?;
    if inner.height < 6 || inner.width < 20 {
        return Some(ShepherdAgentsPanelRender {
            area: outer,
            ..ShepherdAgentsPanelRender::default()
        });
    }

    let stack = crate::ui::modal_stack_areas(inner, 3, 2, 0, 1);
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
    let button_bar_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y.saturating_add(2),
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
    let def_count = panel.definitive_count();
    let subtitle = if needs_you > 0 {
        format!(
            "{} agente(s) · {} esperando al humano · Enter enfoca su terminal",
            panel.entries.len(),
            needs_you
        )
    } else {
        format!(
            "{} agente(s) activos · terminales nativas de shepherd intactas",
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

    // Header action buttons: [s Lanzar] [K Matar] [x Quitar] [X Limpiar (n)]
    let mut btn_x = button_bar_area.x;
    let spawn_btn_rect = Rect::new(btn_x, button_bar_area.y, 14, 1);
    render_button(buffer, spawn_btn_rect, "+ Lanzar (s)", palette.accent, palette);
    btn_x = btn_x.saturating_add(16);

    let kill_btn_rect = Rect::new(btn_x, button_bar_area.y, 13, 1);
    render_button(buffer, kill_btn_rect, "✕ Matar (K)", palette.red, palette);
    btn_x = btn_x.saturating_add(15);

    let clean_btn_rect = Rect::new(btn_x, button_bar_area.y, 14, 1);
    render_button(buffer, clean_btn_rect, "⌫ Quitar (x)", palette.surface_dim, palette);
    btn_x = btn_x.saturating_add(16);

    let clean_all_label = format!("🧹 Limpiar ({def_count}) (X)");
    let clean_all_width = (clean_all_label.chars().count() as u16).max(18);
    let clean_all_btn_rect = Rect::new(btn_x, button_bar_area.y, clean_all_width, 1);
    render_button(buffer, clean_all_btn_rect, &clean_all_label, palette.surface_dim, palette);

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
        let footer_line1 = Line::from(vec![
            Span::styled(" s ", base.fg(palette.accent).add_modifier(Modifier::BOLD)),
            Span::styled("lanzar  ·  ", base.fg(palette.overlay0)),
            Span::styled(" K ", base.fg(palette.red).add_modifier(Modifier::BOLD)),
            Span::styled("matar  ·  ", base.fg(palette.overlay0)),
            Span::styled(" x ", base.fg(palette.yellow).add_modifier(Modifier::BOLD)),
            Span::styled("quitar  ·  ", base.fg(palette.overlay0)),
            Span::styled(" X ", base.fg(palette.mauve).add_modifier(Modifier::BOLD)),
            Span::styled("limpiar definitivos  ·  ", base.fg(palette.overlay0)),
            Span::styled(" Enter ", base.fg(palette.teal).add_modifier(Modifier::BOLD)),
            Span::styled("enfocar pane  ·  ", base.fg(palette.overlay0)),
            Span::styled(" Esc ", base.fg(palette.text).add_modifier(Modifier::BOLD)),
            Span::styled("cerrar", base.fg(palette.overlay0)),
        ]);
        Paragraph::new(footer_line1).render(
            Rect::new(footer_area.x, footer_area.y, footer_area.width, 1),
            buffer,
        );
    }

    let mut confirm_button = Rect::default();
    let mut cancel_button = Rect::default();
    let mut form_fields = Vec::new();

    // Render active sub-modal if present
    match &panel.sub_modal {
        AgentsSubModal::None => {}
        AgentsSubModal::Spawn(form) => {
            let (conf, canc, fields) = render_spawn_modal(buffer, inner, form, palette);
            confirm_button = conf;
            cancel_button = canc;
            form_fields = fields;
        }
        AgentsSubModal::ConfirmKill { agent_id, state: _, message } => {
            let (conf, canc) = render_confirm_modal(
                buffer,
                inner,
                &format!("Confirmación requerida: Matar '{agent_id}'"),
                message,
                " Matar agente ",
                palette.red,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
        AgentsSubModal::ConfirmClean { agent_id, state: _, message } => {
            let (conf, canc) = render_confirm_modal(
                buffer,
                inner,
                &format!("Confirmación requerida: Quitar '{agent_id}'"),
                message,
                " Quitar de lista ",
                palette.yellow,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
        AgentsSubModal::ConfirmCleanAll { count: _, workspace: _, message } => {
            let (conf, canc) = render_confirm_modal(
                buffer,
                inner,
                "Confirmación requerida: Limpiar definitivos",
                message,
                " Limpiar todos ",
                palette.mauve,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
    }

    Some(ShepherdAgentsPanelRender {
        area: outer,
        close_button: close_rect,
        rows: hit_rows,
        scrollbar: track.unwrap_or_default(),
        max_scroll,
        confirm_button,
        cancel_button,
        spawn_button: spawn_btn_rect,
        kill_button: kill_btn_rect,
        clean_button: clean_btn_rect,
        clean_all_button: clean_all_btn_rect,
        form_fields,
    })
}

fn render_button(buffer: &mut Buffer, rect: Rect, label: &str, bg: ratatui::style::Color, palette: &Palette) {
    if rect.width < 2 || rect.height < 1 {
        return;
    }
    let contrast_fg = match bg {
        ratatui::style::Color::Reset => palette.surface_dim,
        color => color,
    };
    let style = Style::default()
        .fg(contrast_fg)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    buffer.set_style(rect, style);
    buffer.set_stringn(rect.x, rect.y, label, usize::from(rect.width), style);
}

fn render_spawn_modal(
    buffer: &mut Buffer,
    container: Rect,
    form: &SpawnFormState,
    palette: &Palette,
) -> (Rect, Rect, Vec<Rect>) {
    let modal_width = 72.min(container.width.saturating_sub(4));
    let modal_height = 16.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_modal_box(buffer, modal_area, palette.panel_bg, palette.accent) {
        Some(r) => r,
        None => return (Rect::default(), Rect::default(), Vec::new()),
    };

    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_stringn(
        inner.x + 1,
        inner.y,
        "Lanzar nuevo agente (Shepherd)",
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.text).add_modifier(Modifier::BOLD),
    );

    let field_names = [
        "Rol (*):",
        "Tarea (*):",
        "Vendor (opc):",
        "Modelo (opc):",
        "Skills extra (separadas por coma):",
    ];
    let field_values = [
        &form.role,
        &form.task,
        &form.vendor,
        &form.model,
        &form.extra_skills,
    ];

    let mut field_rects = Vec::new();
    let mut curr_y = inner.y + 2;

    for idx in 0..5 {
        let is_active = form.active_field == idx;
        let label = field_names[idx];
        let val = field_values[idx];

        buffer.set_stringn(
            inner.x + 1,
            curr_y,
            label,
            usize::from(inner.width.saturating_sub(2)),
            if is_active {
                base.fg(palette.accent).add_modifier(Modifier::BOLD)
            } else {
                base.fg(palette.overlay0)
            },
        );
        curr_y += 1;

        let input_rect = Rect::new(inner.x + 1, curr_y, inner.width.saturating_sub(2), 1);
        let input_style = if is_active {
            Style::default().fg(palette.text).bg(palette.active_row_bg)
        } else {
            Style::default().fg(palette.text).bg(palette.surface_dim)
        };
        for ix in input_rect.x..input_rect.right() {
            buffer[(ix, input_rect.y)].set_symbol(" ").set_style(input_style);
        }
        let display_val = if is_active {
            format!("{val}█")
        } else if val.is_empty() {
            format!("<{}>", label.trim_end_matches(':'))
        } else {
            val.clone()
        };
        buffer.set_stringn(
            input_rect.x,
            input_rect.y,
            &display_val,
            usize::from(input_rect.width),
            if val.is_empty() && !is_active {
                input_style.fg(palette.overlay0)
            } else {
                input_style
            },
        );
        field_rects.push(input_rect);
        curr_y += 1;
    }

    if let Some(ref err) = form.error {
        buffer.set_stringn(
            inner.x + 1,
            curr_y,
            err,
            usize::from(inner.width.saturating_sub(2)),
            base.fg(palette.red).add_modifier(Modifier::BOLD),
        );
    }

    let btn_y = inner.bottom().saturating_sub(1);
    let confirm_btn = Rect::new(inner.x + 2, btn_y, 16, 1);
    let cancel_btn = Rect::new(inner.x + 20, btn_y, 16, 1);

    render_button(buffer, confirm_btn, " ↵ Lanzar ", palette.accent, palette);
    render_button(buffer, cancel_btn, " esc Cancelar ", palette.surface_dim, palette);

    (confirm_btn, cancel_btn, field_rects)
}

fn render_confirm_modal(
    buffer: &mut Buffer,
    container: Rect,
    title: &str,
    message: &str,
    confirm_label: &str,
    confirm_color: ratatui::style::Color,
    palette: &Palette,
) -> (Rect, Rect) {
    let modal_width = 72.min(container.width.saturating_sub(4));
    let modal_height = 9.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_modal_box(buffer, modal_area, palette.panel_bg, confirm_color) {
        Some(r) => r,
        None => return (Rect::default(), Rect::default()),
    };

    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_stringn(
        inner.x + 1,
        inner.y,
        title,
        usize::from(inner.width.saturating_sub(2)),
        base.fg(confirm_color).add_modifier(Modifier::BOLD),
    );

    // Render message lines with wrapping
    let available_width = usize::from(inner.width.saturating_sub(2)).max(10);
    let wrapped = wrap_text(message, available_width);
    for (i, line) in wrapped.into_iter().take(3).enumerate() {
        buffer.set_stringn(
            inner.x + 1,
            inner.y + 2 + i as u16,
            &line,
            available_width,
            base.fg(palette.text),
        );
    }

    let btn_y = inner.bottom().saturating_sub(1);
    let conf_w = (confirm_label.chars().count() as u16).max(14);
    let confirm_btn = Rect::new(inner.x + 2, btn_y, conf_w, 1);
    let cancel_btn = Rect::new(inner.x + 4 + conf_w, btn_y, 16, 1);

    render_button(buffer, confirm_btn, confirm_label, confirm_color, palette);
    render_button(buffer, cancel_btn, " esc Cancelar ", palette.surface_dim, palette);

    (confirm_btn, cancel_btn)
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.is_empty() {
            current_line.push_str(word);
        } else if current_line.chars().count() + 1 + word.chars().count() <= max_width {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    if lines.is_empty() {
        lines.push(text.to_string());
    }
    lines
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
                " No hay agentes de Shepherd registrados en esta sesión. Presiona 's' para lanzar uno.",
                base.fg(palette.overlay1),
            )]),
        )];
    }

    let mut out = Vec::with_capacity(panel.entries.len() * 3);
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

        // Un agente que espera al humano se destaca y su aviso se lee ENTERO (sin truncar)
        if entry.needs_you && !entry.notice.is_empty() {
            let max_notice_w = usize::from(width.saturating_sub(18)).max(20);
            let wrapped_notice = wrap_text(&entry.notice, max_notice_w);
            for (line_idx, nline) in wrapped_notice.into_iter().enumerate() {
                let prefix = if line_idx == 0 {
                    "   ↳ needs you: "
                } else {
                    "                "
                };
                out.push((
                    Some(entry.id.clone()),
                    Line::from(vec![
                        Span::styled(prefix, base.fg(palette.red).add_modifier(Modifier::BOLD)),
                        Span::styled(nline, base.fg(palette.text)),
                    ]),
                ));
            }
        }
    }
    out
}

fn draw_modal_box(
    buffer: &mut Buffer,
    area: Rect,
    bg: ratatui::style::Color,
    border_color: ratatui::style::Color,
) -> Option<Rect> {
    if area.width < 2 || area.height < 2 {
        return None;
    }
    let background = Style::default()
        .bg(bg)
        .remove_modifier(Modifier::DIM);
    let border = Style::default()
        .fg(border_color)
        .bg(bg)
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

    #[test]
    fn test_needs_you_agent_highlighted_and_notice_shown_in_full() {
        let mut reg = AgentRegistryFile::default();
        let long_notice = "Notice: The agent requires approval to execute bash command 'npm install' in workspace w1:p2. Please inspect the command carefully.";
        let mut a = reg.create(
            Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                pane: "w1:p2".into(),
                ..Agent::default()
            },
            "tok".into(),
            "2026-11-03T20:00:00Z",
        );
        a.state = "blocked".into();
        a.notice = long_notice.into();
        reg.agents[0] = a;

        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;
        assert_eq!(panel.needs_you_count(), 1);

        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();
        let rendered = render_shepherd_agents_panel(&mut buffer, area, &panel, &palette)
            .expect("panel should render");
        assert!(!rendered.rows.is_empty());

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("NEEDS YOU"), "Should highlight NEEDS YOU badge");
        assert!(text.contains("approval to execute"), "Should contain part 1 of notice");
        assert!(text.contains("npm"), "Should contain command from notice");
        assert!(text.contains("inspect the command"), "Should contain full wrapped notice text");
    }

    #[test]
    fn test_spawn_form_validations_and_lifecycle() {
        let reg = AgentRegistryFile::default();
        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;

        // Press 's' to open spawn form
        let act = panel.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty()));
        assert_eq!(act, None);
        assert!(matches!(panel.sub_modal, AgentsSubModal::Spawn(_)));

        // Pressing Enter with empty fields fails validation
        let act = panel.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(act, None);
        if let AgentsSubModal::Spawn(ref form) = panel.sub_modal {
            assert!(form.error.is_some());
            assert!(form.error.as_ref().unwrap().contains("rol"));
        } else {
            panic!("Expected spawn form active");
        }

        // Fill role: 'coder'
        for c in "coder".chars() {
            panel.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
        // Tab to task field
        panel.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        // Try enter with empty task
        panel.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        if let AgentsSubModal::Spawn(ref form) = panel.sub_modal {
            assert!(form.error.as_ref().unwrap().contains("tarea"));
        }

        // Fill task: 'fix issue'
        for c in "fix issue".chars() {
            panel.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
        // Tab to vendor field: 'claude'
        panel.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        for c in "claude".chars() {
            panel.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
        // Tab to model field: 'opus'
        panel.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        for c in "opus".chars() {
            panel.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
        // Tab to extra skills: 'rust, git'
        panel.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        for c in "rust, git".chars() {
            panel.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }

        // Submit form with Enter
        let submit_act = panel.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(panel.sub_modal, AgentsSubModal::None);
        assert_eq!(
            submit_act,
            Some(ShepherdAgentsAction::Spawn {
                role: "coder".into(),
                task: "fix issue".into(),
                vendor: Some("claude".into()),
                model: Some("opus".into()),
                extra_skills: vec!["rust".into(), "git".into()],
            })
        );

        // Test cancel with Esc
        panel.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty()));
        assert!(matches!(panel.sub_modal, AgentsSubModal::Spawn(_)));
        let cancel_act = panel.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(cancel_act, None);
        assert_eq!(panel.sub_modal, AgentsSubModal::None);
        assert!(panel.open, "Panel must stay open when submodal is canceled");
    }

    #[test]
    fn test_kill_requires_confirmation_destructive_guard() {
        let mut reg = AgentRegistryFile::default();
        let mut a_done = reg.create(
            Agent {
                role: "coder".into(),
                pane: "w1:p1".into(),
                ..Agent::default()
            },
            "tok".into(),
            "2026-11-03T20:00:00Z",
        );
        a_done.state = "done".into();
        reg.agents[0] = a_done;
        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;

        // Press 'K' to initiate kill
        let init_act = panel.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::empty()));
        // Destructive guard: action must NOT execute without confirmation!
        assert_eq!(init_act, None);
        assert_eq!(panel.last_action, None);

        // Check confirmation message explains done closes tab
        if let AgentsSubModal::ConfirmKill { ref agent_id, ref state, ref message } = panel.sub_modal {
            assert_eq!(agent_id, "coder-1");
            assert_eq!(state, "done");
            assert!(message.contains("cerrará su pestaña"), "Expected explanation of tab closing");
        } else {
            panic!("Expected ConfirmKill submodal");
        }

        // Cancel with Esc
        let esc_act = panel.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(esc_act, None);
        assert_eq!(panel.sub_modal, AgentsSubModal::None);
        assert_eq!(panel.last_action, None, "Kill must not execute after cancel");

        // Press 'K' again and confirm with Enter
        panel.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::empty()));
        let conf_act = panel.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdAgentsAction::Kill {
                agent_id: "coder-1".into()
            })
        );
        assert_eq!(panel.sub_modal, AgentsSubModal::None);
    }

    #[test]
    fn test_clean_requires_confirmation_destructive_guard() {
        let mut reg = AgentRegistryFile::default();
        let mut a = reg.create(
            Agent {
                role: "brain".into(),
                pane: "w1:p1".into(),
                ..Agent::default()
            },
            "tok".into(),
            "2026-11-03T20:00:00Z",
        );
        a.state = "done".into();
        reg.agents[0] = a;
        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;

        // Press 'x' to clean
        let act = panel.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
        // Guard: must NOT execute without confirmation!
        assert_eq!(act, None);
        assert_eq!(panel.last_action, None);

        if let AgentsSubModal::ConfirmClean { ref agent_id, ref message, .. } = panel.sub_modal {
            assert_eq!(agent_id, "brain-1");
            assert!(message.contains("cerrará su pestaña"));
        } else {
            panic!("Expected ConfirmClean submodal");
        }

        // Cancel
        panel.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(panel.sub_modal, AgentsSubModal::None);

        // Press 'x' and confirm
        panel.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
        let conf_act = panel.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdAgentsAction::Clean {
                agent_id: "brain-1".into()
            })
        );
    }

    #[test]
    fn test_clean_all_requires_confirmation_with_count() {
        let mut reg = AgentRegistryFile::default();
        let mut a1 = reg.create(Agent { role: "a".into(), ..Agent::default() }, "t".into(), "now".into());
        a1.state = "working".into();
        reg.agents[0] = a1;

        let mut a2 = reg.create(Agent { role: "b".into(), ..Agent::default() }, "t".into(), "now".into());
        a2.state = "exited".into();
        reg.agents[1] = a2;

        let mut a3 = reg.create(Agent { role: "c".into(), ..Agent::default() }, "t".into(), "now".into());
        a3.state = "killed".into();
        reg.agents[2] = a3;

        let mut a4 = reg.create(Agent { role: "d".into(), ..Agent::default() }, "t".into(), "now".into());
        a4.state = "failed".into();
        reg.agents[3] = a4;

        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;
        assert_eq!(panel.definitive_count(), 3);

        // Press 'X'
        let act = panel.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::empty()));
        assert_eq!(act, None);

        if let AgentsSubModal::ConfirmCleanAll { count, ref message, .. } = panel.sub_modal {
            assert_eq!(count, 3);
            assert!(message.contains("3 agentes definitivos"));
        } else {
            panic!("Expected ConfirmCleanAll submodal");
        }

        // Cancel
        panel.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(panel.sub_modal, AgentsSubModal::None);

        // Press 'X' and confirm
        panel.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::empty()));
        let conf_act = panel.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdAgentsAction::CleanAllFinished {
                count: 3,
                workspace: None,
            })
        );
    }

    #[test]
    fn test_one_modal_at_a_time_and_esc_cancels() {
        let mut reg = AgentRegistryFile::default();
        reg.create(Agent { id: "a1".into(), state: "working".into(), ..Agent::default() }, "t".into(), "now".into());

        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;

        panel.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty()));
        assert!(matches!(panel.sub_modal, AgentsSubModal::Spawn(_)));

        // Try pressing 'K' while spawn form is open: ignored!
        panel.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::empty()));
        assert!(matches!(panel.sub_modal, AgentsSubModal::Spawn(_)));

        // Esc cancels spawn form
        panel.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(panel.sub_modal, AgentsSubModal::None);
        assert!(panel.open);

        // Esc now closes the panel
        panel.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert!(!panel.open);
    }

    #[test]
    fn test_mouse_clicks_and_double_click_focus() {
        let mut reg = AgentRegistryFile::default();
        let _a1 = reg.create(Agent { role: "coder".into(), pane: "p1".into(), ..Agent::default() }, "t".into(), "now".into());
        let _a2 = reg.create(Agent { role: "coder".into(), pane: "p2".into(), ..Agent::default() }, "t".into(), "now".into());

        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;

        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();
        let rendered = render_shepherd_agents_panel(&mut buffer, area, &panel, &palette).unwrap();

        // Click row 1: selects row 1
        let r1 = rendered.rows[1].0;
        let act = panel.handle_click(r1.x + 1, r1.y, &rendered);
        assert_eq!(act, None);
        assert_eq!(panel.selected, 1);

        // Click row 1 again (double click): focuses agent!
        let act = panel.handle_click(r1.x + 1, r1.y, &rendered);
        assert_eq!(
            act,
            Some(ShepherdAgentsAction::FocusPane {
                pane_id: "p2".into(),
                agent_id: "coder-2".into(),
            })
        );
        assert!(!panel.open, "Panel closes after focusing pane");

        // Reopen and test close button click
        panel.open = true;
        let act = panel.handle_click(rendered.close_button.x + 1, rendered.close_button.y, &rendered);
        assert_eq!(act, None);
        assert!(!panel.open, "Close button must close panel");
    }
}
