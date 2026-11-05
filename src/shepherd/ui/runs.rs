use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::components::{
    compact_panel,
    confirm_stripe::{render_confirm_stripe, ConfirmStripeHits},
    detail_view::{
        render_detail_view, AlertLevel, DetailContentBlock, DetailHeader, DetailViewHits,
    },
    draw_divider,
    empty_state::{render_empty_state, EmptyStateHits},
    inline_form::{render_inline_form, FormField, InlineFormHits, InlineFormState},
    list_view::{render_list_view, ListViewHits, ListViewItem},
    split_panes,
};
use crate::app::state::Palette;
pub use crate::shepherd::runs::{
    GateKind, LaunchOptionsFormState, RunStatus, ShepherdRunRow, ShepherdRunsAction,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdRunsHits {
    pub list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub launch_form_hits: Option<InlineFormHits>,
    pub gate_stripe_hits: Option<ConfirmStripeHits>,
    pub cancel_stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdRunsSectionState {
    pub selected: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub runs: Vec<ShepherdRunRow>,
    pub launch_form: Option<LaunchOptionsFormState>,
    pub confirm_cancel: Option<u32>,
    pub last_action: Option<ShepherdRunsAction>,
    pub error: Option<String>,
}

impl Default for ShepherdRunsSectionState {
    fn default() -> Self {
        Self {
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            runs: Vec::new(),
            launch_form: None,
            confirm_cancel: None,
            last_action: None,
            error: None,
        }
    }
}

impl ShepherdRunsSectionState {
    pub fn selected_run(&self) -> Option<&ShepherdRunRow> {
        self.runs.get(self.selected)
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
        if !self.runs.is_empty() && self.selected + 1 < self.runs.len() {
            self.selected += 1;
            self.detail_scroll = 0;
        }
    }

    pub fn start_launch_run(&mut self, task_id: u32, has_saved_plan: bool) {
        self.launch_form = Some(LaunchOptionsFormState::new(task_id, has_saved_plan));
        self.confirm_cancel = None;
        self.error = None;
    }

    pub fn start_cancel_run(&mut self) {
        if let Some(run) = self.selected_run() {
            if matches!(run.status, RunStatus::Running) {
                self.confirm_cancel = Some(run.task_id);
                self.launch_form = None;
                self.error = None;
            }
        }
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.launch_form.is_some() || self.confirm_cancel.is_some() {
            self.launch_form = None;
            self.confirm_cancel = None;
            self.error = None;
            true
        } else {
            false
        }
    }

    pub fn submit_launch(&mut self) -> Option<ShepherdRunsAction> {
        let Some(ref form) = self.launch_form else {
            return None;
        };
        let action = ShepherdRunsAction::LaunchOrResume {
            task_id: form.task_id,
            options: form.options.clone(),
        };
        self.last_action = Some(action.clone());
        self.launch_form = None;
        Some(action)
    }

    pub fn confirm_cancel_run(&mut self) -> Option<ShepherdRunsAction> {
        if let Some(task_id) = self.confirm_cancel.take() {
            let action = ShepherdRunsAction::CancelRun { task_id };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn approve_pending_gate(&mut self) -> Option<ShepherdRunsAction> {
        if let Some(run) = self.selected_run() {
            if let Some(gate) = &run.pending_gate {
                let action = ShepherdRunsAction::ApproveGate {
                    task_id: run.task_id,
                    gate: gate.clone(),
                };
                self.last_action = Some(action.clone());
                return Some(action);
            }
        }
        None
    }

    pub fn reject_pending_gate(&mut self) -> Option<ShepherdRunsAction> {
        if let Some(run) = self.selected_run() {
            if let Some(gate) = &run.pending_gate {
                let action = ShepherdRunsAction::RejectGate {
                    task_id: run.task_id,
                    gate: gate.clone(),
                };
                self.last_action = Some(action.clone());
                return Some(action);
            }
        }
        None
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdRunsAction>) {
        // 1. If launch options form is active
        if let Some(ref mut form) = self.launch_form {
            match key.code {
                KeyCode::Esc => {
                    self.launch_form = None;
                    return (true, None);
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.cycle_forward();
                    return (true, None);
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.cycle_backward();
                    return (true, None);
                }
                KeyCode::Char(' ') | KeyCode::Char('t') => {
                    form.toggle_active();
                    return (true, None);
                }
                KeyCode::Enter => {
                    let act = self.submit_launch();
                    return (true, act);
                }
                _ => return (false, None),
            }
        }

        // 2. If cancel run confirmation stripe is active
        if self.confirm_cancel.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.confirm_cancel = None;
                    return (true, None);
                }
                KeyCode::Enter | KeyCode::Char('c') | KeyCode::Char('y') => {
                    let act = self.confirm_cancel_run();
                    return (true, act);
                }
                _ => return (false, None),
            }
        }

        // 3. Normal view keys (including gate approval shortcuts)
        if let Some(run) = self.selected_run() {
            if run.pending_gate.is_some() {
                if key.code == KeyCode::Char('a') {
                    let act = self.approve_pending_gate();
                    return (true, act);
                }
                if key.code == KeyCode::Char('d') {
                    let act = self.reject_pending_gate();
                    return (true, act);
                }
            }
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                (true, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                (true, None)
            }
            KeyCode::Char('l') | KeyCode::Char('r') => {
                if let Some(run) = self.selected_run() {
                    self.start_launch_run(run.task_id, run.has_saved_plan);
                } else {
                    self.start_launch_run(1, false);
                }
                (true, None)
            }
            KeyCode::Char('c') => {
                self.start_cancel_run();
                (true, None)
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(5);
                (true, None)
            }
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(5);
                (true, None)
            }
            _ => (false, None),
        }
    }
}

pub fn render_runs_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdRunsSectionState,
    palette: &Palette,
) -> ShepherdRunsHits {
    let mut hits = ShepherdRunsHits::default();
    if area.width < 20 || area.height < 5 {
        return hits;
    }

    let list_width = if area.width >= 100 {
        (area.width * 32 / 100).max(28)
    } else if area.width >= 70 {
        (area.width * 35 / 100).max(24)
    } else {
        24
    };

    let list_items: Vec<ListViewItem> = state
        .runs
        .iter()
        .map(|r| {
            let (badge_str, badge_style) = if r.pending_gate.is_some() {
                (
                    "[GATE: PUSH/PR]".to_string(),
                    Style::default()
                        .fg(palette.yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                match &r.status {
                    RunStatus::Running => {
                        ("[RUNNING]".to_string(), Style::default().fg(palette.green))
                    }
                    RunStatus::Completed => (
                        "[COMPLETED]".to_string(),
                        Style::default().fg(palette.overlay0),
                    ),
                    RunStatus::Failed { .. } => {
                        ("[FAILED]".to_string(), Style::default().fg(palette.red))
                    }
                    RunStatus::Cancelled => (
                        "[CANCELLED]".to_string(),
                        Style::default().fg(palette.peach),
                    ),
                }
            };
            ListViewItem {
                title: format!("Run #{} {}", r.task_id, r.task_title),
                badge: Some((badge_str, badge_style)),
                meta: None,
            }
        })
        .collect();

    let split = split_panes(
        area,
        list_width,
        compact_panel(
            state.launch_form.is_some() || state.confirm_cancel.is_some(),
            list_items.is_empty(),
        ),
    );
    let list_area = split.list;
    hits.list_hits = render_list_view(
        buffer,
        list_area,
        &list_items,
        state.selected,
        state.list_scroll,
        None,
        Some(("+ Iniciar", "l")),
        palette,
    );

    if let Some(detail_area) = split.detail {
        if let Some(sep_x) = split.divider_x {
            draw_divider(
                buffer,
                area,
                sep_x,
                Style::default().fg(palette.surface_dim),
            );
        }

        // 1. If launch options form is active
        if let Some(ref form) = state.launch_form {
            let inline_form = InlineFormState {
                title: format!("OPCIONES DE LANZAMIENTO - RUN #{}", form.task_id),
                fields: vec![
                    FormField {
                        label: "Pausar para aprobación humana del plan:".into(),
                        value: if form.options.wait_plan_approval {
                            "[✓] Activado".into()
                        } else {
                            "[ ] Desactivado".into()
                        },
                        placeholder: "Espacio para alternar".into(),
                    },
                    FormField {
                        label: "Publicación automática (auto_push - SIEMPRE false al abrir):"
                            .into(),
                        value: if form.options.auto_push {
                            "[✓] Activado".into()
                        } else {
                            "[ ] Desactivado (seguro)".into()
                        },
                        placeholder: "Espacio para alternar".into(),
                    },
                    FormField {
                        label: "Replanificar desde cero (replan):".into(),
                        value: if form.options.replan {
                            "[✓] Sí (forzar nuevo plan)".into()
                        } else if form.has_saved_plan {
                            "[ ] No (usar plan existente)".into()
                        } else {
                            "[!] No disponible (sin plan guardado)".into()
                        },
                        placeholder: "Espacio para alternar".into(),
                    },
                ],
                active_field: form.active_option,
                error: form.error.clone(),
                success: Some("Parámetros validados correctamente.".into()),
            };

            hits.launch_form_hits = Some(render_inline_form(
                buffer,
                detail_area,
                &inline_form,
                "Iniciar Run (Enter)",
                "Cancelar (Esc)",
                true,
                palette,
            ));
            return hits;
        }

        // 2. Normal run detail view
        if let Some(run) = state.selected_run() {
            let mut badges = vec![];
            if run.pending_gate.is_some() {
                badges.push((
                    "[ESPERANDO PUERTA HUMANA]".into(),
                    Style::default()
                        .fg(palette.yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                let status_name = match &run.status {
                    RunStatus::Running => "RUNNING",
                    RunStatus::Completed => "COMPLETED",
                    RunStatus::Failed { .. } => "FAILED",
                    RunStatus::Cancelled => "CANCELLED",
                };
                let color = match &run.status {
                    RunStatus::Running => palette.green,
                    RunStatus::Completed => palette.overlay0,
                    RunStatus::Failed { .. } => palette.red,
                    RunStatus::Cancelled => palette.peach,
                };
                badges.push((format!("[{status_name}]"), Style::default().fg(color)));
            }

            let meta = format!(
                "Fase actual: {}  |  Subtareas: {}/{}  |  Agentes activos: {}",
                run.current_stage,
                run.subtasks_progress.0,
                run.subtasks_progress.1,
                if run.active_agents.is_empty() {
                    "ninguno".to_string()
                } else {
                    run.active_agents.join(", ")
                }
            );

            let header = DetailHeader {
                title: format!("RUN #{}: {}", run.task_id, run.task_title),
                badges,
                metadata_line: Some(meta),
                actions: vec![],
            };

            let mut content = vec![];

            // If gate pending, notice block
            if let Some(ref gate) = run.pending_gate {
                let gate_msg = match gate {
                    GateKind::PlanApproval => "El planificador ha generado el plan y espera tu aprobación para comenzar a codificar.".to_string(),
                    GateKind::PushAndPr { subtask } => format!("Subtarea #{subtask} completada. Se requiere autorización explícita para publicar rama y crear PR en GitHub."),
                };
                content.push(DetailContentBlock::Notice {
                    text: gate_msg,
                    level: AlertLevel::Warning,
                });
            }

            content.push(DetailContentBlock::Heading("ESTADO DE EJECUCIÓN:".into()));
            content.push(DetailContentBlock::Paragraph(format!(
                "Fase en curso: {}\nPlan guardado disponible: {}\nSubtareas completadas: {} de {}",
                run.current_stage,
                if run.has_saved_plan { "Sí" } else { "No" },
                run.subtasks_progress.0,
                run.subtasks_progress.1,
            )));

            if !run.active_agents.is_empty() {
                content.push(DetailContentBlock::Heading("AGENTES ASIGNADOS:".into()));
                content.push(DetailContentBlock::BulletList(run.active_agents.clone()));
            }

            // Reserve space for gate stripe or cancel stripe if active
            let has_stripe = run.pending_gate.is_some() || state.confirm_cancel.is_some();
            let main_detail_area = if has_stripe {
                Rect::new(
                    detail_area.x,
                    detail_area.y,
                    detail_area.width,
                    detail_area.height.saturating_sub(4),
                )
            } else {
                detail_area
            };

            let bottom_actions = [
                ("Iniciar/Reanudar", "l", true),
                ("Cancelar Run", "c", false),
            ];

            hits.detail_hits = Some(render_detail_view(
                buffer,
                main_detail_area,
                &header,
                &content,
                state.detail_scroll,
                &bottom_actions,
                palette,
            ));

            let stripe_area = Rect::new(
                detail_area.x,
                detail_area.bottom().saturating_sub(4),
                detail_area.width,
                4,
            );

            // Gate approval stripe
            if let Some(ref gate) = run.pending_gate {
                let (warn, conf_label, can_label) = match gate {
                    GateKind::PlanApproval => (
                        "¿Aprobar el plan propuesto para arrancar los agentes?",
                        "✓ Aprobar Plan (a)",
                        "✗ Rechazar Plan (d)",
                    ),
                    GateKind::PushAndPr { .. } => (
                        "🔒 AVISO: Aprobar publicará la rama en el repositorio remoto y creará un PR.",
                        "✓ Aprobar Push & Crear PR (a)",
                        "✗ Rechazar Gate (d)",
                    ),
                };
                hits.gate_stripe_hits = Some(render_confirm_stripe(
                    buffer,
                    stripe_area,
                    warn,
                    conf_label,
                    can_label,
                    false,
                    palette,
                ));
            } else if let Some(cancel_id) = state.confirm_cancel {
                // Cancel confirmation stripe
                hits.cancel_stripe_hits = Some(render_confirm_stripe(
                    buffer,
                    stripe_area,
                    &format!("¿Cancelar Run #{cancel_id}? Los agentes que ya hayan arrancado permanecerán en sus panes."),
                    "Confirmar Cancelar (Enter)",
                    "Volver (Esc)",
                    true,
                    palette,
                ));
            }
        } else {
            // Empty state
            let actions = [("+ Iniciar Primer Run", Some("l"))];
            hits.empty_hits = Some(render_empty_state(
                buffer,
                detail_area,
                "NO HAY EJECUCIONES REGISTRADAS",
                "No hay runs activos ni recientes en este proyecto.\nInicia un run para coordinar agentes sobre tus tareas.",
                &actions,
                palette,
            ));
        }
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_runs_section_navigation_and_cancel_guard() {
        let mut state = ShepherdRunsSectionState::default();
        state.runs = vec![ShepherdRunRow {
            task_id: 101,
            task_title: "Task 101".into(),
            status: RunStatus::Running,
            current_stage: "coding".into(),
            has_saved_plan: true,
            pending_gate: None,
            active_agents: vec!["coder-1".into()],
            subtasks_progress: (1, 3),
        }];

        // Start cancel
        state.start_cancel_run();
        assert_eq!(state.confirm_cancel, Some(101));

        // Esc cancels without cancelling run
        assert!(state.cancel_sub_action());
        assert_eq!(state.confirm_cancel, None);
        assert_eq!(state.last_action, None);

        // Confirm cancel
        state.start_cancel_run();
        let act = state.confirm_cancel_run();
        assert_eq!(act, Some(ShepherdRunsAction::CancelRun { task_id: 101 }));
    }

    #[test]
    fn test_runs_section_gate_confirmation_guards() {
        let mut state = ShepherdRunsSectionState::default();
        state.runs = vec![ShepherdRunRow {
            task_id: 102,
            task_title: "Task 102".into(),
            status: RunStatus::Running,
            current_stage: "waiting_gate".into(),
            has_saved_plan: true,
            pending_gate: Some(GateKind::PushAndPr { subtask: 1 }),
            active_agents: vec![],
            subtasks_progress: (1, 1),
        }];

        // Press 'a' to approve gate
        let (handled, act) =
            state.handle_key(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(handled);
        assert_eq!(
            act,
            Some(ShepherdRunsAction::ApproveGate {
                task_id: 102,
                gate: GateKind::PushAndPr { subtask: 1 },
            })
        );
    }

    #[test]
    fn test_runs_section_launch_options_auto_push_always_false() {
        let mut state = ShepherdRunsSectionState::default();
        state.start_launch_run(10, false);

        let form = state.launch_form.as_ref().unwrap();
        // auto_push must ALWAYS be false upon opening!
        assert_eq!(form.options.auto_push, false);

        // Replan without saved plan is rejected
        let (handled, _) = state.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(handled);
        let (handled, _) = state.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(handled);
        let (handled, _) = state.handle_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(handled);
        assert!(state.launch_form.as_ref().unwrap().error.is_some());
        assert_eq!(state.launch_form.as_ref().unwrap().options.replan, false);
    }
}
