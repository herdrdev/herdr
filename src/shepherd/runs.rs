//! Runs and gates view for Shepherd.
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdRunsState`) separated from runtime/I/O.
//! - Pure render (`render_shepherd_runs_panel`) taking `&ShepherdRunsState` and `&Palette`.
//! - Mouse-first with keyboard shortcuts.
//! - Launch/resume options: `auto_push` is ALWAYS `false` on open.
//! - Replan is only allowed if a plan is saved.
//! - Cancel run warns that already spawned agents stay alive in their panes.
//! - Approve/reject gate ALWAYS requires explicit human confirmation explaining that approve = push & PR.

#![allow(dead_code)]

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::panel::contains_point;
use crate::app::state::Palette;

pub const SHEPHERD_RUNS_MODAL_SIZE: (u16, u16) = (90, 24);

/// Status of a task workflow execution run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed { reason: String },
    Cancelled,
}

/// Gate waiting for human approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateKind {
    PlanApproval,
    PushAndPr { subtask: u32 },
}

/// Options when launching or resuming a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    pub wait_plan_approval: bool,
    /// Must ALWAYS be false upon opening the dialog.
    pub auto_push: bool,
    /// May only be set to true if a saved plan exists.
    pub replan: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            wait_plan_approval: true,
            auto_push: false, // ALWAYS false upon open!
            replan: false,
        }
    }
}

/// State of the launch/resume options modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptionsFormState {
    pub task_id: u32,
    pub has_saved_plan: bool,
    pub options: LaunchOptions,
    pub active_option: usize, // 0: wait_plan, 1: auto_push, 2: replan
    pub error: Option<String>,
}

impl LaunchOptionsFormState {
    pub fn new(task_id: u32, has_saved_plan: bool) -> Self {
        Self {
            task_id,
            has_saved_plan,
            options: LaunchOptions {
                wait_plan_approval: true,
                auto_push: false, // ALWAYS false upon open!
                replan: false,
            },
            active_option: 0,
            error: None,
        }
    }

    pub fn toggle_active(&mut self) {
        match self.active_option {
            0 => self.options.wait_plan_approval = !self.options.wait_plan_approval,
            1 => self.options.auto_push = !self.options.auto_push,
            2 => {
                if self.has_saved_plan {
                    self.options.replan = !self.options.replan;
                    self.error = None;
                } else {
                    self.error =
                        Some("Replanificar solo está disponible si existe un plan guardado".into());
                }
            }
            _ => {}
        }
    }

    pub fn cycle_forward(&mut self) {
        self.active_option = (self.active_option + 1) % 3;
    }

    pub fn cycle_backward(&mut self) {
        self.active_option = if self.active_option == 0 {
            2
        } else {
            self.active_option - 1
        };
    }
}

/// Actions emitted by the runs and gates view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdRunsAction {
    LaunchOrResume {
        task_id: u32,
        options: LaunchOptions,
    },
    CancelRun {
        task_id: u32,
    },
    ApproveGate {
        task_id: u32,
        gate: GateKind,
    },
    RejectGate {
        task_id: u32,
        gate: GateKind,
    },
}

/// Sub-modals for runs and gates. At most one active at a time.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum RunsSubModal {
    #[default]
    None,
    LaunchOptions(LaunchOptionsFormState),
    ConfirmCancelRun {
        task_id: u32,
        message: String,
    },
    ConfirmApproveGate {
        task_id: u32,
        gate: GateKind,
        message: String,
    },
    ConfirmRejectGate {
        task_id: u32,
        gate: GateKind,
        message: String,
    },
}

/// Pure presentation row for a task run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdRunRow {
    pub task_id: u32,
    pub task_title: String,
    pub status: RunStatus,
    pub current_stage: String,
    pub has_saved_plan: bool,
    pub pending_gate: Option<GateKind>,
    pub active_agents: Vec<String>,
    pub subtasks_progress: (usize, usize),
}

impl ShepherdRunRow {
    pub fn from_value(val: &serde_json::Value) -> Option<Self> {
        let task_id = val.get("task_id").and_then(serde_json::Value::as_u64)? as u32;
        let task_title = val
            .get("task_title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let status_str = val
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("pending");
        let status = match status_str {
            "done" | "completed" => RunStatus::Completed,
            "failed" => RunStatus::Failed {
                reason: "failed".into(),
            },
            "cancelled" => RunStatus::Cancelled,
            _ => RunStatus::Running,
        };
        let current_stage = val
            .get("current_stage")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(status_str)
            .to_string();
        let has_saved_plan = val
            .get("has_saved_plan")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let pending_gate = match val.get("pending_gate") {
            Some(serde_json::Value::String(s)) if s == "plan" || s == "approve_plan" => {
                Some(GateKind::PlanApproval)
            }
            Some(serde_json::Value::Object(obj)) => {
                let kind = obj
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let subtask = obj
                    .get("subtask")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32;
                if kind == "push_pr" || subtask > 0 {
                    Some(GateKind::PushAndPr { subtask })
                } else if kind == "plan" {
                    Some(GateKind::PlanApproval)
                } else {
                    None
                }
            }
            _ => None,
        };
        let active_agents = val
            .get("active_agents")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let subtasks_progress = val
            .get("subtasks_progress")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                let done = a.first().and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
                let total = a.get(1).and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
                (done, total)
            })
            .unwrap_or((0, 0));
        Some(Self {
            task_id,
            task_title,
            status,
            current_stage,
            has_saved_plan,
            pending_gate,
            active_agents,
            subtasks_progress,
        })
    }
}

/// Pure presentation state for the runs and gates view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdRunsState {
    pub open: bool,
    pub selected: usize,
    pub scroll: u16,
    pub runs: Vec<ShepherdRunRow>,
    pub sub_modal: RunsSubModal,
    pub last_action: Option<ShepherdRunsAction>,
    pub error: Option<String>,
}

impl ShepherdRunsState {
    pub fn new(runs: Vec<ShepherdRunRow>) -> Self {
        Self {
            open: false,
            selected: 0,
            scroll: 0,
            runs,
            sub_modal: RunsSubModal::None,
            last_action: None,
            error: None,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if !self.open {
            self.sub_modal = RunsSubModal::None;
        }
    }

    pub fn open_panel(&mut self) {
        self.open = true;
    }

    pub fn close_panel(&mut self) {
        self.open = false;
        self.sub_modal = RunsSubModal::None;
    }

    pub fn select_next(&mut self) {
        if !self.runs.is_empty() {
            self.selected = (self.selected + 1).min(self.runs.len().saturating_sub(1));
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_index(&mut self, index: usize) {
        if index < self.runs.len() {
            self.selected = index;
        }
    }

    pub fn selected_run(&self) -> Option<&ShepherdRunRow> {
        self.runs.get(self.selected)
    }

    pub fn open_launch_options(&mut self) {
        if !matches!(self.sub_modal, RunsSubModal::None) {
            return;
        }
        if let Some(run) = self.selected_run() {
            self.sub_modal = RunsSubModal::LaunchOptions(LaunchOptionsFormState::new(
                run.task_id,
                run.has_saved_plan,
            ));
        }
    }

    pub fn open_cancel_confirm(&mut self) {
        if !matches!(self.sub_modal, RunsSubModal::None) {
            return;
        }
        if let Some(run) = self.selected_run() {
            let task_id = run.task_id;
            let message = format!(
                "¿Cancelar el run de la tarea '#{task_id}'? Aviso: los agentes ya lanzados seguirán vivos en sus terminales nativas de shepherd."
            );
            self.sub_modal = RunsSubModal::ConfirmCancelRun { task_id, message };
        }
    }

    pub fn open_approve_gate_confirm(&mut self) {
        if !matches!(self.sub_modal, RunsSubModal::None) {
            return;
        }
        if let Some(run) = self.selected_run() {
            if let Some(ref gate) = run.pending_gate {
                let task_id = run.task_id;
                let message = match gate {
                    GateKind::PlanApproval => {
                        format!("¿Aprobar plan de la tarea '#{task_id}'? Se iniciará la fase de implementación de código.")
                    }
                    GateKind::PushAndPr { subtask } => {
                        format!("¿Aprobar gate de la subtarea {subtask}? Esta acción realizará git push a la rama remota y abrirá el Pull Request en GitHub.")
                    }
                };
                self.sub_modal = RunsSubModal::ConfirmApproveGate {
                    task_id,
                    gate: gate.clone(),
                    message,
                };
            }
        }
    }

    pub fn open_reject_gate_confirm(&mut self) {
        if !matches!(self.sub_modal, RunsSubModal::None) {
            return;
        }
        if let Some(run) = self.selected_run() {
            if let Some(ref gate) = run.pending_gate {
                let task_id = run.task_id;
                let message = match gate {
                    GateKind::PlanApproval => {
                        format!("¿Rechazar plan de la tarea '#{task_id}'? Se solicitará una reformulación del plan.")
                    }
                    GateKind::PushAndPr { subtask } => {
                        format!("¿Rechazar gate de la subtarea {subtask}? No se realizará push ni PR; se solicitará revisión adicional al coder.")
                    }
                };
                self.sub_modal = RunsSubModal::ConfirmRejectGate {
                    task_id,
                    gate: gate.clone(),
                    message,
                };
            }
        }
    }

    pub fn cancel_sub_modal(&mut self) {
        self.sub_modal = RunsSubModal::None;
    }

    pub fn confirm_active_action(&mut self) -> Option<ShepherdRunsAction> {
        match std::mem::replace(&mut self.sub_modal, RunsSubModal::None) {
            RunsSubModal::None => None,
            RunsSubModal::LaunchOptions(form) => {
                if form.options.replan && !form.has_saved_plan {
                    let mut form = form;
                    form.error = Some("No se puede replanificar sin un plan guardado".into());
                    self.sub_modal = RunsSubModal::LaunchOptions(form);
                    None
                } else {
                    let action = ShepherdRunsAction::LaunchOrResume {
                        task_id: form.task_id,
                        options: form.options,
                    };
                    self.last_action = Some(action.clone());
                    Some(action)
                }
            }
            RunsSubModal::ConfirmCancelRun { task_id, .. } => {
                let action = ShepherdRunsAction::CancelRun { task_id };
                self.last_action = Some(action.clone());
                Some(action)
            }
            RunsSubModal::ConfirmApproveGate { task_id, gate, .. } => {
                let action = ShepherdRunsAction::ApproveGate { task_id, gate };
                self.last_action = Some(action.clone());
                Some(action)
            }
            RunsSubModal::ConfirmRejectGate { task_id, gate, .. } => {
                let action = ShepherdRunsAction::RejectGate { task_id, gate };
                self.last_action = Some(action.clone());
                Some(action)
            }
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<ShepherdRunsAction> {
        use crossterm::event::KeyCode;
        match self.sub_modal {
            RunsSubModal::None => match key.code {
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
                KeyCode::Char('r') | KeyCode::Char('l') => {
                    self.open_launch_options();
                    None
                }
                KeyCode::Char('c') => {
                    self.open_cancel_confirm();
                    None
                }
                KeyCode::Char('a') | KeyCode::Char('y') | KeyCode::Char('A') => {
                    self.open_approve_gate_confirm();
                    None
                }
                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('R') => {
                    self.open_reject_gate_confirm();
                    None
                }
                _ => None,
            },
            RunsSubModal::LaunchOptions(ref mut form) => match key.code {
                KeyCode::Esc => {
                    self.cancel_sub_modal();
                    None
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.cycle_forward();
                    None
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.cycle_backward();
                    None
                }
                KeyCode::Char(' ') => {
                    form.toggle_active();
                    None
                }
                KeyCode::Enter => self.confirm_active_action(),
                _ => None,
            },
            RunsSubModal::ConfirmCancelRun { .. }
            | RunsSubModal::ConfirmApproveGate { .. }
            | RunsSubModal::ConfirmRejectGate { .. } => match key.code {
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
        render: &ShepherdRunsRender,
    ) -> Option<ShepherdRunsAction> {
        if contains_point(render.close_button, col, row) {
            self.close_panel();
            return None;
        }

        match self.sub_modal {
            RunsSubModal::None => {
                if contains_point(render.launch_button, col, row) {
                    self.open_launch_options();
                    return None;
                }
                if contains_point(render.cancel_run_button, col, row) {
                    self.open_cancel_confirm();
                    return None;
                }
                if contains_point(render.approve_gate_button, col, row) {
                    self.open_approve_gate_confirm();
                    return None;
                }
                if contains_point(render.reject_gate_button, col, row) {
                    self.open_reject_gate_confirm();
                    return None;
                }
                for (idx, (rect, _task_id)) in render.run_rows.iter().enumerate() {
                    if contains_point(*rect, col, row) {
                        self.select_index(idx);
                        return None;
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
                } else if let RunsSubModal::LaunchOptions(ref mut form) = self.sub_modal {
                    for (opt_idx, opt_rect) in render.option_checkboxes.iter().enumerate() {
                        if contains_point(*opt_rect, col, row) {
                            form.active_option = opt_idx;
                            form.toggle_active();
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

/// Recorded rendering regions for hit testing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdRunsRender {
    pub area: Rect,
    pub close_button: Rect,
    pub run_rows: Vec<(Rect, u32)>,
    pub launch_button: Rect,
    pub cancel_run_button: Rect,
    pub approve_gate_button: Rect,
    pub reject_gate_button: Rect,
    pub confirm_button: Rect,
    pub cancel_button: Rect,
    pub option_checkboxes: Vec<Rect>,
}

/// Pure render function for the runs and gates view.
pub fn render_shepherd_runs_panel(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdRunsState,
    palette: &Palette,
) -> Option<ShepherdRunsRender> {
    if area.height < 6 || area.width < 20 {
        return Some(ShepherdRunsRender {
            area,
            ..ShepherdRunsRender::default()
        });
    }

    let inner = area;
    let stack = crate::ui::modal_stack_areas(inner, 2, 2, 0, 1);
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);

    // Header
    let button_bar = Rect::new(
        stack.header.x + 1,
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );

    // Buttons in header (row 0)
    let mut btn_x = button_bar.x;
    let launch_btn = Rect::new(btn_x, button_bar.y, 16, 1);
    render_runs_btn(
        buffer,
        launch_btn,
        "▶ Lanzar/Reanudar",
        palette.accent,
        palette,
    );
    btn_x = btn_x.saturating_add(18);

    let cancel_run_btn = Rect::new(btn_x, button_bar.y, 14, 1);
    render_runs_btn(
        buffer,
        cancel_run_btn,
        "■ Cancelar run",
        palette.yellow,
        palette,
    );
    btn_x = btn_x.saturating_add(16);

    let approve_gate_btn = Rect::new(btn_x, button_bar.y, 16, 1);
    render_runs_btn(
        buffer,
        approve_gate_btn,
        "✓ Aprobar gate",
        palette.green,
        palette,
    );
    btn_x = btn_x.saturating_add(18);

    let reject_gate_btn = Rect::new(btn_x, button_bar.y, 16, 1);
    render_runs_btn(
        buffer,
        reject_gate_btn,
        "✗ Rechazar gate",
        palette.red,
        palette,
    );

    // Subtitle in header (row 1)
    let sub_title = format!(
        "{} run(s) · human gates protegen push y PR",
        state.runs.len()
    );
    buffer.set_stringn(
        stack.header.x + 1,
        stack.header.y + 1,
        &sub_title,
        usize::from(stack.header.width.saturating_sub(2)),
        base.fg(palette.overlay0),
    );

    // Body content: list of runs with progress and gates
    let body = stack.content;
    let mut run_rows = Vec::new();

    if state.runs.is_empty() {
        buffer.set_stringn(
            body.x + 1,
            body.y + 1,
            "No hay ejecuciones activas ni registradas.",
            usize::from(body.width.saturating_sub(2)),
            base.fg(palette.overlay0),
        );
    } else {
        for (idx, run) in state.runs.iter().enumerate().take(usize::from(body.height)) {
            let row_y = body.y + idx as u16;
            let row_rect = Rect::new(body.x, row_y, body.width, 1);
            let is_sel = idx == state.selected;
            let row_bg = if is_sel {
                palette.active_row_bg
            } else {
                palette.panel_bg
            };
            let row_style = base.bg(row_bg);

            for rx in row_rect.x..row_rect.right() {
                buffer[(rx, row_y)].set_symbol(" ").set_style(row_style);
            }

            let (status_icon, _status_color, status_text) = match &run.status {
                RunStatus::Running => ("●", palette.yellow, "EN CURSO".to_string()),
                RunStatus::Completed => ("●", palette.green, "TERMINADO".to_string()),
                RunStatus::Failed { reason } => ("●", palette.red, format!("FALLIDO: {reason}")),
                RunStatus::Cancelled => ("○", palette.overlay0, "CANCELADO".to_string()),
            };

            let gate_badge = match &run.pending_gate {
                Some(GateKind::PlanApproval) => " [GATE: Espera aprobación plan] ",
                Some(GateKind::PushAndPr { subtask: _ }) => " [GATE: Espera push/PR subtarea] ",
                None => "",
            };

            let row_str = format!(
                "{} #{:<2} {} [{}] subtareas:{}/{}",
                status_icon,
                run.task_id,
                run.task_title,
                status_text,
                run.subtasks_progress.0,
                run.subtasks_progress.1,
            );

            buffer.set_stringn(
                row_rect.x + 1,
                row_y,
                &row_str,
                usize::from(row_rect.width.saturating_sub(2)),
                if is_sel {
                    row_style.fg(palette.text).add_modifier(Modifier::BOLD)
                } else {
                    row_style.fg(palette.text)
                },
            );

            if !gate_badge.is_empty() {
                let badge_x = row_rect
                    .right()
                    .saturating_sub(gate_badge.chars().count() as u16 + 2);
                let badge_style = Style::default()
                    .fg(palette.surface_dim)
                    .bg(palette.red)
                    .add_modifier(Modifier::BOLD);
                buffer.set_stringn(
                    badge_x,
                    row_y,
                    gate_badge,
                    gate_badge.chars().count(),
                    badge_style,
                );
            }

            run_rows.push((row_rect, run.task_id));
        }
    }

    // Footer
    if let Some(footer_area) = stack.footer {
        let footer_line = Line::from(vec![
            Span::styled(
                " r/l ",
                base.fg(palette.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("lanzar/reanudar  ·  ", base.fg(palette.overlay0)),
            Span::styled(" c ", base.fg(palette.yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancelar  ·  ", base.fg(palette.overlay0)),
            Span::styled(" a ", base.fg(palette.green).add_modifier(Modifier::BOLD)),
            Span::styled("aprobar gate  ·  ", base.fg(palette.overlay0)),
            Span::styled(" d ", base.fg(palette.red).add_modifier(Modifier::BOLD)),
            Span::styled("rechazar gate  ·  ", base.fg(palette.overlay0)),
            Span::styled(" Esc ", base.fg(palette.text).add_modifier(Modifier::BOLD)),
            Span::styled("cerrar", base.fg(palette.overlay0)),
        ]);
        Paragraph::new(footer_line).render(
            Rect::new(footer_area.x, footer_area.y, footer_area.width, 1),
            buffer,
        );
    }

    let mut confirm_button = Rect::default();
    let mut cancel_button = Rect::default();
    let mut option_checkboxes = Vec::new();

    // Render submodals
    match &state.sub_modal {
        RunsSubModal::None => {}
        RunsSubModal::LaunchOptions(form) => {
            let (conf, canc, opts) = render_launch_options_modal(buffer, inner, form, palette);
            confirm_button = conf;
            cancel_button = canc;
            option_checkboxes = opts;
        }
        RunsSubModal::ConfirmCancelRun { task_id, message } => {
            let (conf, canc) = render_runs_confirm_modal(
                buffer,
                inner,
                &format!("Confirmación: Cancelar Run #{task_id}"),
                message,
                " Cancelar Run ",
                palette.yellow,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
        RunsSubModal::ConfirmApproveGate {
            task_id,
            gate: _,
            message,
        } => {
            let (conf, canc) = render_runs_confirm_modal(
                buffer,
                inner,
                &format!("Confirmación Requerida: Aprobar Gate #{task_id}"),
                message,
                " Aprobar y Publicar (Push/PR) ",
                palette.green,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
        RunsSubModal::ConfirmRejectGate {
            task_id,
            gate: _,
            message,
        } => {
            let (conf, canc) = render_runs_confirm_modal(
                buffer,
                inner,
                &format!("Confirmación: Rechazar Gate #{task_id}"),
                message,
                " Rechazar Gate ",
                palette.red,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
    }

    Some(ShepherdRunsRender {
        area: inner,
        close_button: Rect::default(),
        run_rows,
        launch_button: launch_btn,
        cancel_run_button: cancel_run_btn,
        approve_gate_button: approve_gate_btn,
        reject_gate_button: reject_gate_btn,
        confirm_button,
        cancel_button,
        option_checkboxes,
    })
}

fn render_launch_options_modal(
    buffer: &mut Buffer,
    container: Rect,
    form: &LaunchOptionsFormState,
    palette: &Palette,
) -> (Rect, Rect, Vec<Rect>) {
    let modal_width = 78.min(container.width.saturating_sub(4));
    let modal_height = 14.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_runs_box(buffer, modal_area, palette.panel_bg, palette.accent) {
        Some(r) => r,
        None => return (Rect::default(), Rect::default(), Vec::new()),
    };

    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_stringn(
        inner.x + 1,
        inner.y,
        format!(
            "Opciones de Lanzamiento / Reanudación (Tarea #{})",
            form.task_id
        ),
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.text).add_modifier(Modifier::BOLD),
    );

    let options = [
        (
            form.options.wait_plan_approval,
            "Esperar aprobación humana del plan (por defecto según configuración)",
        ),
        (
            form.options.auto_push,
            "Auto-push: aprueba push y PR por adelantado (SIEMPRE apagado al abrir)",
        ),
        (
            form.options.replan,
            if form.has_saved_plan {
                "Replanificar: regenerar plan con brain (plan guardado detectado)"
            } else {
                "Replanificar: deshabilitado (no hay plan guardado para esta tarea)"
            },
        ),
    ];

    let mut opt_rects = Vec::new();
    let mut curr_y = inner.y + 2;

    for (idx, (val, text)) in options.iter().enumerate() {
        let is_active = form.active_option == idx;
        let opt_rect = Rect::new(inner.x + 1, curr_y, inner.width.saturating_sub(2), 1);
        let check = if *val { "[x]" } else { "[ ]" };
        let opt_style = if is_active {
            base.bg(palette.active_row_bg).add_modifier(Modifier::BOLD)
        } else {
            base
        };

        for ox in opt_rect.x..opt_rect.right() {
            buffer[(ox, opt_rect.y)]
                .set_symbol(" ")
                .set_style(opt_style);
        }

        let label = format!("{check} {text}");
        buffer.set_stringn(
            opt_rect.x + 1,
            opt_rect.y,
            &label,
            usize::from(opt_rect.width.saturating_sub(2)),
            if is_active {
                opt_style.fg(palette.accent)
            } else {
                opt_style.fg(palette.text)
            },
        );

        opt_rects.push(opt_rect);
        curr_y += 2;
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

    render_runs_btn(
        buffer,
        confirm_btn,
        " ↵ Lanzar run ",
        palette.accent,
        palette,
    );
    render_runs_btn(
        buffer,
        cancel_btn,
        " esc Cancelar ",
        palette.surface_dim,
        palette,
    );

    (confirm_btn, cancel_btn, opt_rects)
}

fn render_runs_confirm_modal(
    buffer: &mut Buffer,
    container: Rect,
    title: &str,
    message: &str,
    confirm_label: &str,
    confirm_color: ratatui::style::Color,
    palette: &Palette,
) -> (Rect, Rect) {
    let modal_width = 76.min(container.width.saturating_sub(4));
    let modal_height = 10.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_runs_box(buffer, modal_area, palette.panel_bg, confirm_color) {
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

    let mut words = message.split_whitespace();
    let mut line1 = String::new();
    let mut line2 = String::new();
    let max_w = usize::from(inner.width.saturating_sub(4));

    for word in words.by_ref() {
        if line1.chars().count() + 1 + word.chars().count() <= max_w {
            if !line1.is_empty() {
                line1.push(' ');
            }
            line1.push_str(word);
        } else {
            line2.push_str(word);
            break;
        }
    }
    for word in words {
        if line2.chars().count() + 1 + word.chars().count() <= max_w {
            if !line2.is_empty() {
                line2.push(' ');
            }
            line2.push_str(word);
        }
    }

    buffer.set_stringn(
        inner.x + 1,
        inner.y + 2,
        &line1,
        max_w,
        base.fg(palette.text),
    );
    if !line2.is_empty() {
        buffer.set_stringn(
            inner.x + 1,
            inner.y + 3,
            &line2,
            max_w,
            base.fg(palette.text),
        );
    }

    let btn_y = inner.bottom().saturating_sub(1);
    let conf_w = (confirm_label.chars().count() as u16).max(16);
    let confirm_btn = Rect::new(inner.x + 2, btn_y, conf_w, 1);
    let cancel_btn = Rect::new(inner.x + 4 + conf_w, btn_y, 16, 1);

    render_runs_btn(buffer, confirm_btn, confirm_label, confirm_color, palette);
    render_runs_btn(
        buffer,
        cancel_btn,
        " esc Cancelar ",
        palette.surface_dim,
        palette,
    );

    (confirm_btn, cancel_btn)
}

fn render_runs_btn(
    buffer: &mut Buffer,
    rect: Rect,
    label: &str,
    bg: ratatui::style::Color,
    palette: &Palette,
) {
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

fn draw_runs_box(
    buffer: &mut Buffer,
    area: Rect,
    bg: ratatui::style::Color,
    border_color: ratatui::style::Color,
) -> Option<Rect> {
    if area.width < 2 || area.height < 2 {
        return None;
    }
    let background = Style::default().bg(bg).remove_modifier(Modifier::DIM);
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

    #[test]
    fn test_launch_options_auto_push_always_false_on_open_and_replan_guard() {
        let run = ShepherdRunRow {
            task_id: 1,
            task_title: "Workflow run".into(),
            status: RunStatus::Running,
            current_stage: "plan".into(),
            has_saved_plan: false, // NO saved plan!
            pending_gate: None,
            active_agents: vec!["brain-1".into()],
            subtasks_progress: (0, 3),
        };
        let mut state = ShepherdRunsState::new(vec![run]);
        state.open = true;

        // Open launch options
        state.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty()));
        assert!(matches!(state.sub_modal, RunsSubModal::LaunchOptions(_)));

        // Guard: auto_push must ALWAYS be false when dialog is opened
        if let RunsSubModal::LaunchOptions(ref form) = state.sub_modal {
            assert!(!form.options.auto_push, "auto_push must be false on open");
            assert!(
                form.options.wait_plan_approval,
                "wait_plan_approval defaults to true"
            );
            assert!(!form.options.replan, "replan must be false by default");
        } else {
            panic!("Expected launch options modal");
        }

        // Try toggling replan when has_saved_plan is false
        if let RunsSubModal::LaunchOptions(ref mut form) = state.sub_modal {
            form.active_option = 2; // replan
            form.toggle_active();
            assert!(
                !form.options.replan,
                "Replan must NOT toggle without saved plan"
            );
            assert!(form.error.as_ref().unwrap().contains("plan guardado"));
        }

        // Now test with saved plan
        let run_with_plan = ShepherdRunRow {
            task_id: 2,
            task_title: "Planned run".into(),
            status: RunStatus::Running,
            current_stage: "code".into(),
            has_saved_plan: true,
            pending_gate: None,
            active_agents: vec!["coder-1".into()],
            subtasks_progress: (1, 3),
        };
        let mut state_planned = ShepherdRunsState::new(vec![run_with_plan]);
        state_planned.open = true;
        state_planned.open_launch_options();

        if let RunsSubModal::LaunchOptions(ref mut form) = state_planned.sub_modal {
            assert!(
                !form.options.auto_push,
                "auto_push must still be false on open"
            );
            form.active_option = 2; // replan
            form.toggle_active();
            assert!(form.options.replan, "Replan toggles when saved plan exists");
        }

        // Submit launch options
        let act = state_planned.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            act,
            Some(ShepherdRunsAction::LaunchOrResume {
                task_id: 2,
                options: LaunchOptions {
                    wait_plan_approval: true,
                    auto_push: false,
                    replan: true,
                },
            })
        );
    }

    #[test]
    fn test_cancel_run_destructive_guard_and_notice() {
        let run = ShepherdRunRow {
            task_id: 5,
            task_title: "Active run".into(),
            status: RunStatus::Running,
            current_stage: "code".into(),
            has_saved_plan: true,
            pending_gate: None,
            active_agents: vec!["coder-1".into()],
            subtasks_progress: (1, 2),
        };
        let mut state = ShepherdRunsState::new(vec![run]);
        state.open = true;

        // Press 'c' to cancel
        let act = state.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()));
        // Destructive guard: action must NOT execute without confirmation!
        assert_eq!(act, None);
        assert_eq!(state.last_action, None);

        if let RunsSubModal::ConfirmCancelRun {
            task_id,
            ref message,
        } = state.sub_modal
        {
            assert_eq!(task_id, 5);
            assert!(
                message.contains("agentes ya lanzados seguirán vivos"),
                "Cancel run must warn that running agents remain alive"
            );
        } else {
            panic!("Expected ConfirmCancelRun modal");
        }

        // Cancel with Esc
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(state.sub_modal, RunsSubModal::None);
        assert_eq!(state.last_action, None);

        // Press 'c' and confirm with Enter
        state.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()));
        let conf_act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(conf_act, Some(ShepherdRunsAction::CancelRun { task_id: 5 }));
        assert_eq!(state.sub_modal, RunsSubModal::None);
    }

    #[test]
    fn test_gate_approval_requires_confirmation_explaining_push_and_pr() {
        let run = ShepherdRunRow {
            task_id: 10,
            task_title: "Gate awaiting PR".into(),
            status: RunStatus::Running,
            current_stage: "review".into(),
            has_saved_plan: true,
            pending_gate: Some(GateKind::PushAndPr { subtask: 2 }),
            active_agents: vec!["reviewer-1".into()],
            subtasks_progress: (1, 2),
        };
        let mut state = ShepherdRunsState::new(vec![run]);
        state.open = true;

        // Press 'a' to approve gate
        let act = state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
        // Publishing action guard: must NOT push or PR without explicit confirmation!
        assert_eq!(act, None);
        assert_eq!(state.last_action, None);

        if let RunsSubModal::ConfirmApproveGate {
            task_id,
            ref gate,
            ref message,
        } = state.sub_modal
        {
            assert_eq!(task_id, 10);
            assert_eq!(*gate, GateKind::PushAndPr { subtask: 2 });
            assert!(
                message.contains("git push") && message.contains("Pull Request"),
                "Approve gate text must explicitly explain that approving triggers push and PR"
            );
        } else {
            panic!("Expected ConfirmApproveGate modal");
        }

        // Cancel with Esc
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(state.sub_modal, RunsSubModal::None);
        assert_eq!(state.last_action, None);

        // Confirm approval with 'a' + Enter
        state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
        let conf_act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdRunsAction::ApproveGate {
                task_id: 10,
                gate: GateKind::PushAndPr { subtask: 2 },
            })
        );
        assert_eq!(state.sub_modal, RunsSubModal::None);
    }

    #[test]
    fn test_gate_rejection_requires_confirmation() {
        let run = ShepherdRunRow {
            task_id: 12,
            task_title: "Plan review".into(),
            status: RunStatus::Running,
            current_stage: "plan".into(),
            has_saved_plan: true,
            pending_gate: Some(GateKind::PlanApproval),
            active_agents: vec!["brain-1".into()],
            subtasks_progress: (0, 1),
        };
        let mut state = ShepherdRunsState::new(vec![run]);
        state.open = true;

        // Press 'd' to reject gate
        let act = state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()));
        assert_eq!(act, None);

        if let RunsSubModal::ConfirmRejectGate {
            task_id, ref gate, ..
        } = state.sub_modal
        {
            assert_eq!(task_id, 12);
            assert_eq!(*gate, GateKind::PlanApproval);
        } else {
            panic!("Expected ConfirmRejectGate modal");
        }

        // Cancel
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(state.sub_modal, RunsSubModal::None);

        // Confirm reject with 'd' + Enter
        state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()));
        let conf_act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdRunsAction::RejectGate {
                task_id: 12,
                gate: GateKind::PlanApproval,
            })
        );
    }

    #[test]
    fn test_pure_render_runs_panel() {
        let run = ShepherdRunRow {
            task_id: 3,
            task_title: "Render runs test".into(),
            status: RunStatus::Running,
            current_stage: "code".into(),
            has_saved_plan: true,
            pending_gate: Some(GateKind::PushAndPr { subtask: 1 }),
            active_agents: vec!["coder-1".into()],
            subtasks_progress: (1, 2),
        };
        let state = ShepherdRunsState::new(vec![run]);
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let rendered = render_shepherd_runs_panel(&mut buffer, area, &state, &palette)
            .expect("should render runs panel");
        assert!(!rendered.run_rows.is_empty());

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("human gates"));
        assert!(text.contains("Render runs test"));
        assert!(text.contains("EN CURSO"));
        assert!(text.contains("GATE:"));
    }
}
