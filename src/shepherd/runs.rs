//! Runs and gates view for Shepherd.
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdRunsState`) separated from runtime/I/O.
//! - Mouse-first with keyboard shortcuts.
//! - Launch/resume options: `auto_push` is ALWAYS `false` on open.
//! - Replan is only allowed if a plan is saved.
//! - Cancel run warns that already spawned agents stay alive in their panes.
//! - Approve/reject gate ALWAYS requires explicit human confirmation explaining that approve = push & PR.

#![allow(dead_code)]

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
}
