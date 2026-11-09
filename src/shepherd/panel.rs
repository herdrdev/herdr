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
    Refresh,
    /// Snapshot timeline request (F11c).
    Snapshots(crate::shepherd::ui::snapshots::ShepherdSnapshotsAction),
}

/// Sub-modals displayed within the Shepherd agents panel.
/// Only one sub-modal or confirmation may be active at any given time.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum AgentsSubModal {
    #[default]
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
            return Err("The role is required".into());
        }
        if self.task.trim().is_empty() {
            return Err("The task description is required".into());
        }
        Ok(())
    }

    pub fn cycle_field_forward(&mut self) {
        self.active_field = (self.active_field + 1) % 5;
    }

    pub fn cycle_field_backward(&mut self) {
        self.active_field = if self.active_field == 0 {
            4
        } else {
            self.active_field - 1
        };
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
            0 => {
                self.role.pop();
            }
            1 => {
                self.task.pop();
            }
            2 => {
                self.vendor.pop();
            }
            3 => {
                self.model.pop();
            }
            4 => {
                self.extra_skills.pop();
            }
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
    pub worktree: String,
    pub root: String,
    pub snapshots: u32,
    pub last_snapshot: String,
}

impl ShepherdAgentRow {
    pub fn from_agent(agent: &shepherd_core::Agent) -> Self {
        let notice = agent.notice.trim().to_owned();
        let needs_you =
            matches!(agent.state.as_str(), "blocked" | "needs-human") || !notice.is_empty();
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
            worktree: agent.worktree.clone(),
            root: agent.root.clone(),
            snapshots: agent.snapshots,
            last_snapshot: agent.last_snapshot.clone(),
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
                format!("Kill agent '{id}'? The agent is 'done'; its tab will be closed.")
            } else {
                format!("Kill agent '{id}'? Status: {state}. Its process will be terminated and it will be marked as killed.")
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
                format!("Remove agent '{id}' from the list? Status: done; its tab will be closed and its session removed.")
            } else {
                format!("Remove agent '{id}' from the list? Status: {state}. Its session record and worktree will be removed.")
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
            "Clean the {count} final agents of the workspace (exited, killed, failed)? Their leftover panes will be closed."
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
            AgentsSubModal::ConfirmCleanAll {
                count, workspace, ..
            } => {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
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
            assert!(form.error.as_ref().unwrap().contains("task"));
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
        if let AgentsSubModal::ConfirmKill {
            ref agent_id,
            ref state,
            ref message,
        } = panel.sub_modal
        {
            assert_eq!(agent_id, "coder-1");
            assert_eq!(state, "done");
            assert!(
                message.contains("its tab will be closed"),
                "Expected explanation of tab closing"
            );
        } else {
            panic!("Expected ConfirmKill submodal");
        }

        // Cancel with Esc
        let esc_act = panel.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(esc_act, None);
        assert_eq!(panel.sub_modal, AgentsSubModal::None);
        assert_eq!(
            panel.last_action, None,
            "Kill must not execute after cancel"
        );

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

        if let AgentsSubModal::ConfirmClean {
            ref agent_id,
            ref message,
            ..
        } = panel.sub_modal
        {
            assert_eq!(agent_id, "brain-1");
            assert!(message.contains("its tab will be closed"));
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
        let mut a1 = reg.create(
            Agent {
                role: "a".into(),
                ..Agent::default()
            },
            "t".into(),
            "now",
        );
        a1.state = "working".into();
        reg.agents[0] = a1;

        let mut a2 = reg.create(
            Agent {
                role: "b".into(),
                ..Agent::default()
            },
            "t".into(),
            "now",
        );
        a2.state = "exited".into();
        reg.agents[1] = a2;

        let mut a3 = reg.create(
            Agent {
                role: "c".into(),
                ..Agent::default()
            },
            "t".into(),
            "now",
        );
        a3.state = "killed".into();
        reg.agents[2] = a3;

        let mut a4 = reg.create(
            Agent {
                role: "d".into(),
                ..Agent::default()
            },
            "t".into(),
            "now",
        );
        a4.state = "failed".into();
        reg.agents[3] = a4;

        let mut panel = ShepherdAgentsPanelState::from_registry(&reg);
        panel.open = true;
        assert_eq!(panel.definitive_count(), 3);

        // Press 'X'
        let act = panel.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::empty()));
        assert_eq!(act, None);

        if let AgentsSubModal::ConfirmCleanAll {
            count, ref message, ..
        } = panel.sub_modal
        {
            assert_eq!(count, 3);
            assert!(message.contains("3 final agents"));
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
        reg.create(
            Agent {
                id: "a1".into(),
                state: "working".into(),
                ..Agent::default()
            },
            "t".into(),
            "now",
        );

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
}
