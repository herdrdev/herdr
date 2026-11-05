//! Skills view for Shepherd.
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdSkillsState`) separated from runtime/I/O.
//! - Mouse-first with keyboard shortcuts.
//! - Three tiers per agent: Global / Role / Session, displaying origin scope and edited flag.
//! - Notice clearly states: adding/removing skills applies on agent RESTART; text edits apply live.
//! - Restoring an edited skill requires confirmation.
//! - Catalog operations: create, edit, deactivate, make global, delete (user/repo only; default is protected).
//! - Shadowed configuration changes fail with an error message instead of pretending success.

#![allow(dead_code)]

use shepherd_core::skills::{Level, Scope};

/// Mode of the skills view: viewing an agent's assigned skills vs viewing the global catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SkillsViewMode {
    AgentView,
    #[default]
    CatalogView,
}

/// A skill assigned to an agent across the 3 tiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSkillItem {
    pub name: String,
    pub description: String,
    pub level: Level, // Global, Role, Session
    pub scope: Scope, // Default, User, Repo
    pub active: bool,
    pub edited: bool,
    pub shadowed_by: Option<String>,
}

/// A skill in the repository/user/default catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSkillItem {
    pub name: String,
    pub description: String,
    pub scope: Scope, // Default, User, Repo
    pub is_global: bool,
    pub enabled: bool,
    pub problems: Vec<String>,
}

impl From<&shepherd_core::skills::Skill> for CatalogSkillItem {
    fn from(s: &shepherd_core::skills::Skill) -> Self {
        Self {
            name: s.name.clone(),
            description: s.description.clone(),
            scope: s.scope,
            is_global: s.global,
            enabled: s.ok(),
            problems: s.problems.clone(),
        }
    }
}

/// Form state for creating a new catalog skill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateSkillFormState {
    pub name: String,
    pub description: String,
    pub scope: Scope,
    pub is_global: bool,
    pub error: Option<String>,
}

impl Default for CreateSkillFormState {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            scope: Scope::User,
            is_global: false,
            error: None,
        }
    }
}

impl CreateSkillFormState {
    pub fn validate(&self) -> Result<(), String> {
        let n = self.name.trim();
        if n.is_empty() {
            return Err("El nombre de la skill es obligatorio".into());
        }
        if !shepherd_core::skills::name_ok(n) {
            return Err(format!(
                "Nombre de skill inválido: '{n}'. Use solo minúsculas, números y guiones."
            ));
        }
        if self.description.trim().is_empty() {
            return Err("La descripción de la skill es obligatoria".into());
        }
        Ok(())
    }
}

/// Actions emitted by the skills view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdSkillsAction {
    ToggleAgentSkill {
        agent_id: String,
        skill_name: String,
        enable: bool,
    },
    RestoreEditedSkill {
        agent_id: String,
        skill_name: String,
    },
    CreateCatalogSkill {
        name: String,
        description: String,
        scope: Scope,
        is_global: bool,
    },
    ToggleCatalogGlobal {
        name: String,
        make_global: bool,
    },
    DeleteCatalogSkill {
        name: String,
        scope: Scope,
    },
}

/// Sub-modals for skills. At most one active at a time.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SkillsSubModal {
    #[default]
    None,
    Create(CreateSkillFormState),
    ConfirmRestore {
        agent_id: String,
        skill_name: String,
        message: String,
    },
    ConfirmDelete {
        skill_name: String,
        scope: Scope,
        message: String,
    },
}

/// Pure presentation state for the skills view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdSkillsState {
    pub open: bool,
    pub mode: SkillsViewMode,
    pub selected_agent_id: String,
    pub agent_role: String,
    pub selected: usize,
    pub scroll: u16,
    pub agent_skills: Vec<AgentSkillItem>,
    pub catalog_skills: Vec<CatalogSkillItem>,
    pub sub_modal: SkillsSubModal,
    pub last_action: Option<ShepherdSkillsAction>,
    pub error: Option<String>,
    pub feedback_notice: Option<String>,
}

impl ShepherdSkillsState {
    pub fn new(
        agent_id: &str,
        agent_role: &str,
        agent_skills: Vec<AgentSkillItem>,
        catalog_skills: Vec<CatalogSkillItem>,
    ) -> Self {
        let mode = if agent_id.trim().is_empty() {
            SkillsViewMode::CatalogView
        } else {
            SkillsViewMode::AgentView
        };
        Self {
            open: false,
            mode,
            selected_agent_id: agent_id.to_string(),
            agent_role: agent_role.to_string(),
            selected: 0,
            scroll: 0,
            agent_skills,
            catalog_skills,
            sub_modal: SkillsSubModal::None,
            last_action: None,
            error: None,
            feedback_notice: None,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if !self.open {
            self.sub_modal = SkillsSubModal::None;
        }
    }

    pub fn open_panel(&mut self) {
        self.open = true;
    }

    pub fn close_panel(&mut self) {
        self.open = false;
        self.sub_modal = SkillsSubModal::None;
    }

    pub fn switch_mode(&mut self) {
        if self.selected_agent_id.trim().is_empty() {
            self.mode = SkillsViewMode::CatalogView;
            return;
        }
        self.mode = match self.mode {
            SkillsViewMode::AgentView => SkillsViewMode::CatalogView,
            SkillsViewMode::CatalogView => SkillsViewMode::AgentView,
        };
        self.selected = 0;
        self.scroll = 0;
        self.error = None;
    }

    pub fn select_next(&mut self) {
        let max_idx = match self.mode {
            SkillsViewMode::AgentView => self.agent_skills.len(),
            SkillsViewMode::CatalogView => self.catalog_skills.len(),
        };
        if max_idx > 0 {
            self.selected = (self.selected + 1).min(max_idx.saturating_sub(1));
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_index(&mut self, index: usize) {
        let max_idx = match self.mode {
            SkillsViewMode::AgentView => self.agent_skills.len(),
            SkillsViewMode::CatalogView => self.catalog_skills.len(),
        };
        if index < max_idx {
            self.selected = index;
        }
    }

    pub fn selected_agent_skill(&self) -> Option<&AgentSkillItem> {
        self.agent_skills.get(self.selected)
    }

    pub fn selected_catalog_skill(&self) -> Option<&CatalogSkillItem> {
        self.catalog_skills.get(self.selected)
    }

    /// Toggle skill activation for the current agent.
    /// Fails with an error message if the change is shadowed by another configuration layer.
    pub fn toggle_current_agent_skill(&mut self) -> Result<Option<ShepherdSkillsAction>, String> {
        let Some(skill) = self.agent_skills.get_mut(self.selected) else {
            return Ok(None);
        };

        // Shadowing check: un cambio que no tenga efecto debe fallar con un mensaje claro
        if let Some(ref blocker) = skill.shadowed_by {
            let msg = format!(
                "No se puede alternar la skill '{}': la configuración de nivel '{}' la anula y tapa este cambio",
                skill.name, blocker
            );
            self.error = Some(msg.clone());
            return Err(msg);
        }

        let new_state = !skill.active;
        skill.active = new_state;

        let action = ShepherdSkillsAction::ToggleAgentSkill {
            agent_id: self.selected_agent_id.clone(),
            skill_name: skill.name.clone(),
            enable: new_state,
        };
        self.last_action = Some(action.clone());
        self.error = None;
        self.feedback_notice = Some(
            "Cambio guardado. Recuerda: añadir o quitar skills se aplica al REINICIAR el agente."
                .into(),
        );
        Ok(Some(action))
    }

    pub fn open_restore_confirm(&mut self) {
        if !matches!(self.sub_modal, SkillsSubModal::None) {
            return;
        }
        if let Some(skill) = self.selected_agent_skill() {
            if !skill.edited {
                self.error = Some(format!(
                    "La skill '{}' no tiene modificaciones locales que restaurar",
                    skill.name
                ));
                return;
            }
            let message = format!(
                "¿Restaurar la skill '{}' al contenido original? Se perderán las modificaciones locales de esta sesión.",
                skill.name
            );
            self.sub_modal = SkillsSubModal::ConfirmRestore {
                agent_id: self.selected_agent_id.clone(),
                skill_name: skill.name.clone(),
                message,
            };
        }
    }

    pub fn open_delete_catalog_confirm(&mut self) {
        if !matches!(self.sub_modal, SkillsSubModal::None) {
            return;
        }
        if let Some(skill) = self.selected_catalog_skill() {
            // Guard: default skills cannot be deleted! Only user and repo skills can be deleted.
            if skill.scope == Scope::Default {
                self.error = Some("No se pueden borrar las skills predeterminadas del sistema (solo usuario/repo)".into());
                return;
            }

            let message = format!(
                "¿Borrar la skill '{}' del catálogo ({})? Se eliminarán los archivos del directorio de la skill.",
                skill.name, skill.scope
            );
            self.sub_modal = SkillsSubModal::ConfirmDelete {
                skill_name: skill.name.clone(),
                scope: skill.scope,
                message,
            };
        }
    }

    pub fn open_create_skill_modal(&mut self) {
        if matches!(self.sub_modal, SkillsSubModal::None) {
            self.sub_modal = SkillsSubModal::Create(CreateSkillFormState {
                scope: Scope::User,
                ..Default::default()
            });
        }
    }

    pub fn toggle_catalog_global(&mut self) -> Option<ShepherdSkillsAction> {
        let skill = self.catalog_skills.get_mut(self.selected)?;
        let new_val = !skill.is_global;
        skill.is_global = new_val;
        let action = ShepherdSkillsAction::ToggleCatalogGlobal {
            name: skill.name.clone(),
            make_global: new_val,
        };
        self.last_action = Some(action.clone());
        Some(action)
    }

    pub fn cancel_sub_modal(&mut self) {
        self.sub_modal = SkillsSubModal::None;
    }

    pub fn confirm_active_action(&mut self) -> Option<ShepherdSkillsAction> {
        match std::mem::replace(&mut self.sub_modal, SkillsSubModal::None) {
            SkillsSubModal::None => None,
            SkillsSubModal::Create(form) => match form.validate() {
                Ok(()) => {
                    let action = ShepherdSkillsAction::CreateCatalogSkill {
                        name: form.name.trim().to_string(),
                        description: form.description.trim().to_string(),
                        scope: form.scope,
                        is_global: form.is_global,
                    };
                    self.last_action = Some(action.clone());
                    Some(action)
                }
                Err(err) => {
                    let mut form = form;
                    form.error = Some(err);
                    self.sub_modal = SkillsSubModal::Create(form);
                    None
                }
            },
            SkillsSubModal::ConfirmRestore {
                agent_id,
                skill_name,
                ..
            } => {
                let action = ShepherdSkillsAction::RestoreEditedSkill {
                    agent_id,
                    skill_name,
                };
                self.last_action = Some(action.clone());
                Some(action)
            }
            SkillsSubModal::ConfirmDelete {
                skill_name, scope, ..
            } => {
                let action = ShepherdSkillsAction::DeleteCatalogSkill {
                    name: skill_name,
                    scope,
                };
                self.last_action = Some(action.clone());
                Some(action)
            }
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<ShepherdSkillsAction> {
        use crossterm::event::KeyCode;
        match self.sub_modal {
            SkillsSubModal::None => match key.code {
                KeyCode::Esc => {
                    self.close_panel();
                    None
                }
                KeyCode::Tab | KeyCode::Char('v') => {
                    self.switch_mode();
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
                KeyCode::Char(' ') => {
                    if self.mode == SkillsViewMode::AgentView {
                        let _ = self.toggle_current_agent_skill();
                        self.last_action.clone()
                    } else {
                        self.toggle_catalog_global()
                    }
                }
                KeyCode::Char('r') => {
                    if self.mode == SkillsViewMode::AgentView {
                        self.open_restore_confirm();
                    }
                    None
                }
                KeyCode::Char('n') => {
                    self.open_create_skill_modal();
                    None
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if self.mode == SkillsViewMode::CatalogView {
                        self.open_delete_catalog_confirm();
                    }
                    None
                }
                KeyCode::Char('g') => {
                    if self.mode == SkillsViewMode::CatalogView {
                        self.toggle_catalog_global()
                    } else {
                        None
                    }
                }
                _ => None,
            },
            SkillsSubModal::Create(ref mut form) => match key.code {
                KeyCode::Esc => {
                    self.cancel_sub_modal();
                    None
                }
                KeyCode::Enter => self.confirm_active_action(),
                KeyCode::Backspace => {
                    form.name.pop();
                    None
                }
                KeyCode::Char(c) => {
                    form.name.push(c);
                    None
                }
                _ => None,
            },
            SkillsSubModal::ConfirmRestore { .. } | SkillsSubModal::ConfirmDelete { .. } => {
                match key.code {
                    KeyCode::Esc => {
                        self.cancel_sub_modal();
                        None
                    }
                    KeyCode::Enter => self.confirm_active_action(),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn test_shadowed_skill_toggle_fails_with_descriptive_error() {
        let skills = vec![AgentSkillItem {
            name: "code-review".into(),
            description: "Review checklists".into(),
            level: Level::Role,
            scope: Scope::Default,
            active: true,
            edited: false,
            shadowed_by: Some("repo".into()), // Shadowed by repository level!
        }];

        let mut state = ShepherdSkillsState::new("coder-1", "coder", skills, Vec::new());
        state.open = true;

        // Press Space to toggle
        let res = state.toggle_current_agent_skill();
        assert!(
            res.is_err(),
            "Must fail when change has no effect due to shadowing"
        );
        let err = res.err().unwrap();
        assert!(
            err.contains("repo"),
            "Error must name the higher layer that shadows it"
        );
        assert!(state.error.is_some());
    }

    #[test]
    fn test_restore_edited_skill_requires_confirmation_destructive_guard() {
        let skills = vec![AgentSkillItem {
            name: "implement-task".into(),
            description: "Focused tasks".into(),
            level: Level::Role,
            scope: Scope::Default,
            active: true,
            edited: true, // Edited!
            shadowed_by: None,
        }];

        let mut state = ShepherdSkillsState::new("coder-1", "coder", skills, Vec::new());
        state.open = true;

        // Press 'r' to restore
        let act = state.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty()));
        // Destructive guard: action must NOT execute without confirmation!
        assert_eq!(act, None);
        assert_eq!(state.last_action, None);

        if let SkillsSubModal::ConfirmRestore {
            ref skill_name,
            ref message,
            ..
        } = state.sub_modal
        {
            assert_eq!(skill_name, "implement-task");
            assert!(message.contains("Se perderán las modificaciones locales"));
        } else {
            panic!("Expected ConfirmRestore modal");
        }

        // Cancel with Esc
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(state.sub_modal, SkillsSubModal::None);
        assert_eq!(state.last_action, None);

        // Confirm restore with Enter
        state.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty()));
        let conf_act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdSkillsAction::RestoreEditedSkill {
                agent_id: "coder-1".into(),
                skill_name: "implement-task".into(),
            })
        );
        assert_eq!(state.sub_modal, SkillsSubModal::None);
    }

    #[test]
    fn test_delete_catalog_skill_protects_default_and_requires_confirmation_for_user_repo() {
        let catalog = vec![
            CatalogSkillItem {
                name: "conventional-commits".into(),
                description: "Default skill".into(),
                scope: Scope::Default, // Protected!
                is_global: true,
                enabled: true,
                problems: Vec::new(),
            },
            CatalogSkillItem {
                name: "my-custom-skill".into(),
                description: "User skill".into(),
                scope: Scope::User, // Deletable!
                is_global: false,
                enabled: true,
                problems: Vec::new(),
            },
        ];

        let mut state = ShepherdSkillsState::new("coder-1", "coder", Vec::new(), catalog);
        state.open = true;
        state.mode = SkillsViewMode::CatalogView;

        // Selected is index 0: "conventional-commits" (Default)
        // Try deleting default skill -> rejected!
        state.open_delete_catalog_confirm();
        assert_eq!(
            state.sub_modal,
            SkillsSubModal::None,
            "Default skills cannot enter delete confirmation"
        );
        assert!(state.error.as_ref().unwrap().contains("predeterminadas"));

        // Select index 1: "my-custom-skill" (User)
        state.select_next();
        assert_eq!(state.selected, 1);

        // Press 'd' to delete user skill
        let act = state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()));
        // Destructive guard: action must NOT execute without confirmation!
        assert_eq!(act, None);
        assert_eq!(state.last_action, None);

        if let SkillsSubModal::ConfirmDelete {
            ref skill_name,
            scope,
            ref message,
        } = state.sub_modal
        {
            assert_eq!(skill_name, "my-custom-skill");
            assert_eq!(scope, Scope::User);
            assert!(message.contains("eliminarán los archivos"));
        } else {
            panic!("Expected ConfirmDelete modal");
        }

        // Cancel with Esc
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(state.sub_modal, SkillsSubModal::None);
        assert_eq!(state.last_action, None);

        // Confirm delete with Enter
        state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()));
        let conf_act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdSkillsAction::DeleteCatalogSkill {
                name: "my-custom-skill".into(),
                scope: Scope::User,
            })
        );
        assert_eq!(state.sub_modal, SkillsSubModal::None);
    }

    #[test]
    fn test_delete_shortcut_accepts_both_lower_and_upper_case() {
        let catalog = vec![CatalogSkillItem {
            name: "custom".into(),
            description: "User skill".into(),
            scope: Scope::User,
            is_global: false,
            enabled: true,
            problems: Vec::new(),
        }];
        let mut state = ShepherdSkillsState::new("", "", Vec::new(), catalog);
        state.open = true;

        // Press 'D' (uppercase)
        state.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::empty()));
        assert!(
            matches!(state.sub_modal, SkillsSubModal::ConfirmDelete { .. }),
            "Uppercase 'D' must trigger delete confirm"
        );
    }
}
