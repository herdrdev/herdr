use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
use super::components::{
    confirm_stripe::{render_confirm_stripe, ConfirmStripeHits},
    detail_view::{render_detail_view, AlertLevel, DetailContentBlock, DetailHeader, DetailViewHits},
    empty_state::{render_empty_state, EmptyStateHits},
    inline_form::{render_inline_form, FormField, InlineFormHits, InlineFormState},
    list_view::{render_list_view, ListViewHits, ListViewItem},
};
pub use crate::shepherd::panel::{ShepherdAgentRow, ShepherdAgentsAction};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdAgentsHits {
    pub list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub form_hits: Option<InlineFormHits>,
    pub stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdAgentsSectionState {
    pub selected: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub agents: Vec<ShepherdAgentRow>,
    pub spawn_form: Option<InlineFormState>,
    pub confirm_kill: Option<String>,
    pub confirm_clean: Option<String>,
    pub confirm_clean_all: Option<usize>,
    pub last_action: Option<ShepherdAgentsAction>,
    pub error: Option<String>,
}

impl Default for ShepherdAgentsSectionState {
    fn default() -> Self {
        Self {
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            agents: Vec::new(),
            spawn_form: None,
            confirm_kill: None,
            confirm_clean: None,
            confirm_clean_all: None,
            last_action: None,
            error: None,
        }
    }
}

impl ShepherdAgentsSectionState {
    pub fn new(agents: Vec<ShepherdAgentRow>) -> Self {
        Self {
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            agents,
            spawn_form: None,
            confirm_kill: None,
            confirm_clean: None,
            confirm_clean_all: None,
            last_action: None,
            error: None,
        }
    }

    pub fn has_sub_action(&self) -> bool {
        self.spawn_form.is_some()
            || self.confirm_kill.is_some()
            || self.confirm_clean.is_some()
            || self.confirm_clean_all.is_some()
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.has_sub_action() {
            self.spawn_form = None;
            self.confirm_kill = None;
            self.confirm_clean = None;
            self.confirm_clean_all = None;
            self.error = None;
            true
        } else {
            false
        }
    }

    pub fn selected_agent(&self) -> Option<&ShepherdAgentRow> {
        self.agents.get(self.selected)
    }

    pub fn needs_you_count(&self) -> usize {
        self.agents.iter().filter(|a| a.needs_you).count()
    }

    pub fn definitive_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| matches!(a.state.as_str(), "exited" | "killed" | "failed" | "done"))
            .count()
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.detail_scroll = 0;
            if self.selected < self.list_scroll {
                self.list_scroll = self.selected;
            }
        }
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() && self.selected + 1 < self.agents.len() {
            self.selected += 1;
            self.detail_scroll = 0;
        }
    }

    pub fn start_spawn(&mut self) {
        self.spawn_form = Some(InlineFormState::new(
            "NUEVO AGENTE (Formulario en línea)",
            vec![
                FormField::new("Rol obligatorio", "ej. coder"),
                FormField::new("Tarea asignada (ID o descripción)", "ej. 12 - Soporte para banderas booleanas"),
                FormField::new("Vendor / Modelo (opcional)", "ej. anthropic/claude-3-7-sonnet"),
                FormField::new("Worktree / Directorio (opcional)", "ej. .shepherd/worktrees/task-12"),
                FormField::new("Skills adicionales (separadas por coma)", "ej. git-safe, rust-tools"),
            ],
        ));
        self.confirm_kill = None;
        self.confirm_clean = None;
        self.confirm_clean_all = None;
        self.error = None;
    }

    pub fn start_kill(&mut self) {
        if let Some(agent) = self.selected_agent() {
            self.confirm_kill = Some(agent.id.clone());
            self.confirm_clean = None;
            self.confirm_clean_all = None;
            self.spawn_form = None;
            self.error = None;
        }
    }

    pub fn start_clean(&mut self) {
        if let Some(agent) = self.selected_agent() {
            self.confirm_clean = Some(agent.id.clone());
            self.confirm_kill = None;
            self.confirm_clean_all = None;
            self.spawn_form = None;
            self.error = None;
        }
    }

    pub fn start_clean_all(&mut self) {
        let count = self.definitive_count();
        if count == 0 {
            self.error = Some("No hay agentes finalizados para limpiar".into());
        } else {
            self.confirm_clean_all = Some(count);
            self.confirm_kill = None;
            self.confirm_clean = None;
            self.spawn_form = None;
            self.error = None;
        }
    }

    pub fn focus_selected_agent(&mut self) -> Option<ShepherdAgentsAction> {
        let agent = self.selected_agent()?;
        if agent.pane.is_empty() {
            self.error = Some("El agente no tiene un terminal asignado".into());
            return None;
        }
        let action = ShepherdAgentsAction::FocusPane {
            pane_id: agent.pane.clone(),
            agent_id: agent.id.clone(),
        };
        self.last_action = Some(action.clone());
        Some(action)
    }

    pub fn submit_spawn(&mut self) -> Result<Option<ShepherdAgentsAction>, String> {
        let Some(ref form) = self.spawn_form else {
            return Ok(None);
        };
        let role = form.fields[0].value.trim().to_string();
        let task = form.fields[1].value.trim().to_string();
        let vendor_raw = form.fields[2].value.trim().to_string();
        let worktree_raw = form.fields[3].value.trim().to_string();
        let skills_raw = form.fields[4].value.trim().to_string();

        if role.is_empty() {
            return Err("El rol del agente es obligatorio".into());
        }
        if task.is_empty() {
            return Err("La tarea asignada es obligatoria".into());
        }

        let (vendor, model) = if vendor_raw.is_empty() {
            (None, None)
        } else if let Some((v, m)) = vendor_raw.split_once('/') {
            (Some(v.trim().to_string()), Some(m.trim().to_string()))
        } else {
            (Some(vendor_raw), None)
        };

        let _worktree = if worktree_raw.is_empty() {
            None
        } else {
            Some(worktree_raw)
        };

        let extra_skills: Vec<String> = if skills_raw.is_empty() {
            Vec::new()
        } else {
            skills_raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        let action = ShepherdAgentsAction::Spawn {
            role,
            task,
            vendor,
            model,
            extra_skills,
        };
        self.last_action = Some(action.clone());
        self.spawn_form = None;
        Ok(Some(action))
    }

    pub fn confirm_kill_action(&mut self) -> Option<ShepherdAgentsAction> {
        if let Some(agent_id) = self.confirm_kill.take() {
            let action = ShepherdAgentsAction::Kill { agent_id };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn confirm_clean_action(&mut self) -> Option<ShepherdAgentsAction> {
        if let Some(agent_id) = self.confirm_clean.take() {
            let action = ShepherdAgentsAction::Clean { agent_id };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn confirm_clean_all_action(&mut self) -> Option<ShepherdAgentsAction> {
        if let Some(count) = self.confirm_clean_all.take() {
            let action = ShepherdAgentsAction::CleanAllFinished {
                count,
                workspace: None,
            };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn update_from_agents(&mut self, agents: &[shepherd_core::Agent]) {
        self.agents = agents.iter().map(ShepherdAgentRow::from_agent).collect();
        if self.selected >= self.agents.len() {
            self.selected = self.agents.len().saturating_sub(1);
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdAgentsAction>) {
        // 1. Confirm kill active
        if self.confirm_kill.is_some() {
            match key.code {
                KeyCode::Enter => {
                    let act = self.confirm_kill_action();
                    return (true, act);
                }
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 2. Confirm clean active
        if self.confirm_clean.is_some() {
            match key.code {
                KeyCode::Enter => {
                    let act = self.confirm_clean_action();
                    return (true, act);
                }
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 3. Confirm clean all active
        if self.confirm_clean_all.is_some() {
            match key.code {
                KeyCode::Enter => {
                    let act = self.confirm_clean_all_action();
                    return (true, act);
                }
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 4. Spawn form active
        if let Some(ref mut form) = self.spawn_form {
            match key.code {
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                KeyCode::Tab => {
                    form.cycle_forward();
                    return (true, None);
                }
                KeyCode::BackTab => {
                    form.cycle_backward();
                    return (true, None);
                }
                KeyCode::Enter => {
                    match self.submit_spawn() {
                        Ok(act) => return (true, act),
                        Err(e) => {
                            if let Some(ref mut f) = self.spawn_form {
                                f.error = Some(e);
                            }
                            return (true, None);
                        }
                    }
                }
                KeyCode::Backspace => {
                    let idx = form.active_field;
                    if idx < form.fields.len() {
                        form.fields[idx].value.pop();
                    }
                    return (true, None);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) => {
                    let idx = form.active_field;
                    if idx < form.fields.len() {
                        form.fields[idx].value.push(c);
                    }
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 5. Normal view navigation and shortcuts
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                (true, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                (true, None)
            }
            KeyCode::Enter => {
                let act = self.focus_selected_agent();
                (true, act)
            }
            KeyCode::Char('s') => {
                self.start_spawn();
                (true, None)
            }
            KeyCode::Char('K') => {
                self.start_kill();
                (true, None)
            }
            KeyCode::Char('x') => {
                self.start_clean();
                (true, None)
            }
            KeyCode::Char('X') => {
                self.start_clean_all();
                (true, None)
            }
            _ => (false, None),
        }
    }
}

pub fn render_agents_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdAgentsSectionState,
    palette: &Palette,
) -> ShepherdAgentsHits {
    let mut hits = ShepherdAgentsHits::default();
    if area.width < 10 || area.height < 4 {
        return hits;
    }

    let is_compact = area.width < 60;
    let list_w = if is_compact {
        area.width
    } else {
        (area.width * 34 / 100).max(24).min(area.width.saturating_sub(24))
    };
    let list_area = Rect::new(area.x, area.y, list_w, area.height);

    let items: Vec<ListViewItem> = state
        .agents
        .iter()
        .map(|a| {
            let (state_color, state_str) = match a.state.as_str() {
                "working" | "running" => (palette.green, "[WORKING]"),
                "blocked" | "needs-human" => (palette.yellow, "[BLOCKED]"),
                "done" | "completed" => (palette.overlay0, "[DONE]"),
                "failed" => (palette.red, "[FAILED]"),
                "killed" => (palette.red, "[KILLED]"),
                _ => (palette.overlay0, "[IDLE]"),
            };
            let meta = if a.needs_you {
                Some("⚠ NEEDS YOU".to_string())
            } else {
                Some(a.vendor.clone())
            };
            ListViewItem {
                title: format!("{:<10} ({})", a.role, a.id),
                badge: Some((state_str.to_string(), Style::default().fg(state_color))),
                meta,
            }
        })
        .collect();

    hits.list_hits = render_list_view(
        buffer,
        list_area,
        &items,
        state.selected,
        state.list_scroll,
        None,
        Some(("+ Nuevo", "s")),
        palette,
    );

    if is_compact {
        return hits;
    }

    // Vertical divider
    let div_x = list_area.right();
    for y in area.y..area.bottom() {
        buffer[(div_x, y)]
            .set_char('│')
            .set_fg(palette.surface_dim);
    }

    let detail_x = div_x + 1;
    let detail_w = area.right().saturating_sub(detail_x);
    let detail_area = Rect::new(detail_x, area.y, detail_w, area.height);

    // 1. Spawn inline form
    if let Some(ref form) = state.spawn_form {
        hits.form_hits = Some(render_inline_form(
            buffer,
            detail_area,
            form,
            "Lanzar Agente",
            "Cancelar",
            true,
            palette,
        ));
        return hits;
    }

    // 2. Confirm kill stripe
    if let Some(ref agent_id) = state.confirm_kill {
        let msg = format!("¿Enviar SIGKILL al proceso del agente '{agent_id}'? Se detendrá inmediatamente.");
        hits.stripe_hits = Some(render_confirm_stripe(
            buffer,
            detail_area,
            &msg,
            "Confirmar Matar",
            "Cancelar",
            true,
            palette,
        ));
        return hits;
    }

    // 3. Confirm clean stripe
    if let Some(ref agent_id) = state.confirm_clean {
        let msg = format!("¿Limpiar el agente '{agent_id}'? Se cerrará su terminal y se eliminarán sus archivos de sesión.");
        hits.stripe_hits = Some(render_confirm_stripe(
            buffer,
            detail_area,
            &msg,
            "Confirmar Limpiar",
            "Cancelar",
            true,
            palette,
        ));
        return hits;
    }

    // 4. Confirm clean all stripe
    if let Some(count) = state.confirm_clean_all {
        let msg = format!("¿Limpiar todos los agentes finalizados ({count})? Se eliminarán de la sesión.");
        hits.stripe_hits = Some(render_confirm_stripe(
            buffer,
            detail_area,
            &msg,
            "Limpiar Definitivos",
            "Cancelar",
            true,
            palette,
        ));
        return hits;
    }

    // 5. Empty state
    if state.agents.is_empty() {
        hits.empty_hits = Some(render_empty_state(
            buffer,
            detail_area,
            "NO HAY AGENTES ACTIVOS",
            "No hay ningún agente en ejecución en esta sesión de Shepherd.\nPuedes lanzar el primer agente con un rol y tarea específicos.",
            &[("+ Lanzar Primer Agente", Some("s"))],
            palette,
        ));
        return hits;
    }

    // 6. Normal Detail view
    if let Some(agent) = state.selected_agent() {
        let (state_color, state_str) = match agent.state.as_str() {
            "working" | "running" => (palette.green, "[WORKING]"),
            "blocked" | "needs-human" => (palette.yellow, "[BLOCKED]"),
            "done" | "completed" => (palette.overlay0, "[DONE]"),
            "failed" => (palette.red, "[FAILED]"),
            "killed" => (palette.red, "[KILLED]"),
            _ => (palette.overlay0, "[IDLE]"),
        };

        let mut badges = vec![
            (state_str.to_string(), Style::default().fg(state_color)),
            (format!("[{}]", agent.role), Style::default().fg(palette.accent)),
        ];
        if agent.needs_you {
            badges.push(("[NEEDS YOU]".to_string(), Style::default().fg(palette.yellow).add_modifier(Modifier::BOLD)));
        }

        let meta_line = format!(
            "Vendor: {} | Modelo: {} | Terminal Pane: {}",
            agent.vendor,
            if agent.model.is_empty() { "default" } else { &agent.model },
            if agent.pane.is_empty() { "ninguno" } else { &agent.pane }
        );

        let header = DetailHeader {
            title: format!("AGENTE: {} (ID: {})", agent.role, agent.id),
            badges,
            metadata_line: Some(meta_line),
            actions: vec![
                ("Ir a Terminal".to_string(), Some("Enter".into())),
                ("+ Nuevo".to_string(), Some("s".into())),
            ],
        };

        let mut blocks = Vec::new();

        if agent.needs_you {
            blocks.push(DetailContentBlock::Notice {
                text: format!(
                    "AVISO DESTACADO (NEEDS YOU):\nEsperando al humano: {}",
                    if agent.notice.is_empty() {
                        "Acción requerida en el terminal nativo (diálogo, login o confirmación)."
                    } else {
                        &agent.notice
                    }
                ),
                level: AlertLevel::Warning,
            });
        }

        blocks.push(DetailContentBlock::Heading("TAREA ASIGNADA:".into()));
        blocks.push(DetailContentBlock::Paragraph(
            if agent.task_text.is_empty() {
                "Sin descripción de tarea asignada.".into()
            } else {
                agent.task_text.clone()
            },
        ));

        let mut bottom_actions = vec![
            ("Ir a Terminal", "Enter", true),
            ("Matar Agente", "K", false),
            ("Limpiar Sesión", "x", false),
        ];

        if state.definitive_count() > 0 {
            bottom_actions.push(("Limpiar Definitivos", "X", false));
        }

        hits.detail_hits = Some(render_detail_view(
            buffer,
            detail_area,
            &header,
            &blocks,
            state.detail_scroll,
            &bottom_actions,
            palette,
        ));
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_agents_section_pure_render_and_empty_state() {
        let state = ShepherdAgentsSectionState::default();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        let area = buffer.area;
        let hits = render_agents_section(&mut buffer, area, &state, &test_palette());
        assert!(hits.empty_hits.is_some());
        assert!(hits.list_hits.action_button.is_some());
    }

    #[test]
    fn test_agents_section_navigation_and_selection() {
        let mut state = ShepherdAgentsSectionState::new(vec![
            ShepherdAgentRow {
                id: "ag-1".into(),
                role: "coder".into(),
                vendor: "claude".into(),
                model: "sonnet".into(),
                state: "working".into(),
                needs_you: false,
                notice: "".into(),
                pane: "pane-1".into(),
                task_text: "Task 1".into(),
            },
            ShepherdAgentRow {
                id: "ag-2".into(),
                role: "reviewer".into(),
                vendor: "claude".into(),
                model: "sonnet".into(),
                state: "blocked".into(),
                needs_you: true,
                notice: "Awaiting approval".into(),
                pane: "pane-2".into(),
                task_text: "Task 2".into(),
            },
        ]);

        assert_eq!(state.selected, 0);
        state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_agent().unwrap().id, "ag-2");
        assert_eq!(state.needs_you_count(), 1);
    }

    #[test]
    fn test_agents_section_destructive_guards_kill_and_clean() {
        let mut state = ShepherdAgentsSectionState::new(vec![
            ShepherdAgentRow {
                id: "ag-1".into(),
                role: "coder".into(),
                vendor: "claude".into(),
                model: "sonnet".into(),
                state: "working".into(),
                needs_you: false,
                notice: "".into(),
                pane: "pane-1".into(),
                task_text: "Task 1".into(),
            },
        ]);

        // 1. Kill requires confirmation
        state.handle_key(&KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE));
        assert_eq!(state.confirm_kill, Some("ag-1".into()));
        assert!(state.has_sub_action());

        // Esc cancels
        state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.confirm_kill.is_none());

        // Kill confirmed with Enter
        state.handle_key(&KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE));
        let (_, act) = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            act,
            Some(ShepherdAgentsAction::Kill {
                agent_id: "ag-1".into()
            })
        );

        // 2. Clean requires confirmation
        state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(state.confirm_clean, Some("ag-1".into()));
        assert!(state.has_sub_action());

        // Clean confirmed with Enter
        let (_, act) = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            act,
            Some(ShepherdAgentsAction::Clean {
                agent_id: "ag-1".into()
            })
        );
    }

    #[test]
    fn test_agents_section_spawn_inline_form_validation() {
        let mut state = ShepherdAgentsSectionState::default();
        state.handle_key(&KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(state.spawn_form.is_some());

        // Trying to submit empty form fails
        let res = state.submit_spawn();
        assert!(res.is_err());

        // Fill role and task
        if let Some(ref mut form) = state.spawn_form {
            form.fields[0].value = "coder".into();
            form.fields[1].value = "Fix issue #12".into();
        }
        let act = state.submit_spawn().unwrap();
        assert_eq!(
            act,
            Some(ShepherdAgentsAction::Spawn {
                role: "coder".into(),
                task: "Fix issue #12".into(),
                vendor: None,
                model: None,
                extra_skills: Vec::new(),
            })
        );
    }
}
