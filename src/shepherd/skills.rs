//! Skills view for Shepherd.
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdSkillsState`) separated from runtime/I/O.
//! - Pure render (`render_shepherd_skills_panel`) taking `&ShepherdSkillsState` and `&Palette`.
//! - Mouse-first with keyboard shortcuts.
//! - Three tiers per agent: Global / Role / Session, displaying origin scope and edited flag.
//! - Notice clearly states: adding/removing skills applies on agent RESTART; text edits apply live.
//! - Restoring an edited skill requires confirmation.
//! - Catalog operations: create, edit, deactivate, make global, delete (user/repo only; default is protected).
//! - Shadowed configuration changes fail with an error message instead of pretending success.

#![allow(dead_code)]

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use shepherd_core::skills::{Level, Scope};

use super::panel::contains_point;
use crate::app::state::Palette;

pub const SHEPHERD_SKILLS_MODAL_SIZE: (u16, u16) = (92, 26);

/// Mode of the skills view: viewing an agent's assigned skills vs viewing the global catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillsViewMode {
    AgentView,
    CatalogView,
}

impl Default for SkillsViewMode {
    fn default() -> Self {
        Self::CatalogView
    }
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillsSubModal {
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

impl Default for SkillsSubModal {
    fn default() -> Self {
        Self::None
    }
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

    pub fn handle_click(
        &mut self,
        col: u16,
        row: u16,
        render: &ShepherdSkillsRender,
    ) -> Option<ShepherdSkillsAction> {
        if contains_point(render.close_button, col, row) {
            self.close_panel();
            return None;
        }

        match self.sub_modal {
            SkillsSubModal::None => {
                if contains_point(render.view_toggle_button, col, row) {
                    self.switch_mode();
                    return None;
                }
                if contains_point(render.restore_button, col, row) {
                    self.open_restore_confirm();
                    return None;
                }
                if contains_point(render.new_skill_button, col, row) {
                    self.open_create_skill_modal();
                    return None;
                }
                if contains_point(render.delete_skill_button, col, row) {
                    self.open_delete_catalog_confirm();
                    return None;
                }
                for (idx, (rect, _name)) in render.skill_rows.iter().enumerate() {
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
                } else {
                    None
                }
            }
        }
    }
}

/// Recorded rendering layout and hit targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdSkillsRender {
    pub area: Rect,
    pub close_button: Rect,
    pub view_toggle_button: Rect,
    pub new_skill_button: Rect,
    pub restore_button: Rect,
    pub delete_skill_button: Rect,
    pub skill_rows: Vec<(Rect, String)>,
    pub confirm_button: Rect,
    pub cancel_button: Rect,
}

/// Pure render function for the skills view.
pub fn render_shepherd_skills_panel(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdSkillsState,
    palette: &Palette,
) -> Option<ShepherdSkillsRender> {
    if area.height < 6 || area.width < 20 {
        return Some(ShepherdSkillsRender {
            area,
            ..ShepherdSkillsRender::default()
        });
    }

    let inner = area;
    let stack = crate::ui::modal_stack_areas(inner, 4, 2, 0, 1);
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);

    // Buttons on row 0
    let button_bar_area = Rect::new(
        stack.header.x + 1,
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );
    let mut btn_x = button_bar_area.x;
    let mode_label = match state.mode {
        SkillsViewMode::AgentView => "Ver Catálogo (v)",
        SkillsViewMode::CatalogView => {
            if state.selected_agent_id.trim().is_empty() {
                "Catálogo (v)"
            } else {
                "Ver Agente (v)"
            }
        }
    };
    let toggle_mode_btn = Rect::new(btn_x, button_bar_area.y, 16, 1);
    render_skills_btn(buffer, toggle_mode_btn, mode_label, palette.blue, palette);
    btn_x = btn_x.saturating_add(18);

    let new_skill_btn = Rect::new(btn_x, button_bar_area.y, 18, 1);
    render_skills_btn(
        buffer,
        new_skill_btn,
        "+ Nueva skill (n)",
        palette.accent,
        palette,
    );
    btn_x = btn_x.saturating_add(20);

    let restore_btn = Rect::new(btn_x, button_bar_area.y, 16, 1);
    render_skills_btn(
        buffer,
        restore_btn,
        "↺ Restaurar (r)",
        palette.teal,
        palette,
    );
    btn_x = btn_x.saturating_add(18);

    let delete_btn = Rect::new(btn_x, button_bar_area.y, 14, 1);
    render_skills_btn(buffer, delete_btn, "🗑 Borrar (d)", palette.red, palette);

    // Context / Mode on row 1
    let mode_text = match state.mode {
        SkillsViewMode::AgentView => {
            if state.selected_agent_id.trim().is_empty() {
                "Catálogo Completo (Repo > User > Default)".to_string()
            } else {
                format!(
                    "Agente '{}' (rol: {})",
                    state.selected_agent_id, state.agent_role
                )
            }
        }
        SkillsViewMode::CatalogView => "Catálogo Completo (Repo > User > Default)".to_string(),
    };
    buffer.set_stringn(
        stack.header.x + 1,
        stack.header.y + 1,
        &mode_text,
        usize::from(stack.header.width.saturating_sub(2)),
        base.fg(palette.text).add_modifier(Modifier::BOLD),
    );

    // Notice on rows 2 & 3
    buffer.set_stringn(
        stack.header.x + 1,
        stack.header.y + 2,
        "Nota: Añadir o quitar skills a este agente se aplicará al REINICIAR el agente.",
        usize::from(stack.header.width.saturating_sub(2)),
        base.fg(palette.yellow),
    );
    buffer.set_stringn(
        stack.header.x + 1,
        stack.header.y + 3,
        "Modificar el texto de una skill sí llega en vivo.",
        usize::from(stack.header.width.saturating_sub(2)),
        base.fg(palette.teal),
    );

    // Body content
    let body = stack.content;
    let mut skill_rows = Vec::new();

    match state.mode {
        SkillsViewMode::AgentView => {
            if state.agent_skills.is_empty() {
                buffer.set_stringn(
                    body.x + 1,
                    body.y + 1,
                    "No hay skills asignadas a este agente.",
                    usize::from(body.width.saturating_sub(2)),
                    base.fg(palette.overlay0),
                );
            } else {
                for (idx, skill) in state
                    .agent_skills
                    .iter()
                    .enumerate()
                    .take(usize::from(body.height))
                {
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

                    let active_icon = if skill.active { "[x]" } else { "[ ]" };
                    let edited_badge = if skill.edited { " (editada)" } else { "" };
                    let level_str = skill.level.as_str();
                    let scope_str = skill.scope.as_str();

                    let text = format!(
                        "{} {:<22}  nivel:{:<7}  origen:{:<7} {}{}",
                        active_icon,
                        skill.name,
                        level_str,
                        scope_str,
                        skill.description,
                        edited_badge,
                    );

                    buffer.set_stringn(
                        row_rect.x + 1,
                        row_y,
                        &text,
                        usize::from(row_rect.width.saturating_sub(2)),
                        if is_sel {
                            row_style.fg(palette.text).add_modifier(Modifier::BOLD)
                        } else {
                            row_style.fg(palette.text)
                        },
                    );

                    skill_rows.push((row_rect, skill.name.clone()));
                }
            }
        }
        SkillsViewMode::CatalogView => {
            if state.catalog_skills.is_empty() {
                buffer.set_stringn(
                    body.x + 1,
                    body.y + 1,
                    "No hay skills disponibles en el catálogo.",
                    usize::from(body.width.saturating_sub(2)),
                    base.fg(palette.overlay0),
                );
            } else {
                for (idx, skill) in state
                    .catalog_skills
                    .iter()
                    .enumerate()
                    .take(usize::from(body.height))
                {
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

                    let global_icon = if skill.is_global {
                        "[GLOBAL]"
                    } else {
                        "        "
                    };
                    let scope_str = skill.scope.as_str();

                    let text = format!(
                        "{} {:<20}  origen:{:<7}  {}",
                        global_icon, skill.name, scope_str, skill.description,
                    );

                    buffer.set_stringn(
                        row_rect.x + 1,
                        row_y,
                        &text,
                        usize::from(row_rect.width.saturating_sub(2)),
                        if is_sel {
                            row_style.fg(palette.text).add_modifier(Modifier::BOLD)
                        } else {
                            row_style.fg(palette.text)
                        },
                    );

                    skill_rows.push((row_rect, skill.name.clone()));
                }
            }
        }
    }

    // Footer lines
    if let Some(footer_area) = stack.footer {
        if let Some(ref err) = state.error {
            buffer.set_stringn(
                footer_area.x + 1,
                footer_area.y,
                err,
                usize::from(footer_area.width.saturating_sub(2)),
                base.fg(palette.red).add_modifier(Modifier::BOLD),
            );
        } else if let Some(ref notice) = state.feedback_notice {
            buffer.set_stringn(
                footer_area.x + 1,
                footer_area.y,
                notice,
                usize::from(footer_area.width.saturating_sub(2)),
                base.fg(palette.teal),
            );
        }

        let footer_line = Line::from(vec![
            Span::styled(
                " Space ",
                base.fg(palette.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("alternar/activar  ·  ", base.fg(palette.overlay0)),
            Span::styled(" v ", base.fg(palette.blue).add_modifier(Modifier::BOLD)),
            Span::styled("cambiar vista  ·  ", base.fg(palette.overlay0)),
            Span::styled(" r ", base.fg(palette.teal).add_modifier(Modifier::BOLD)),
            Span::styled("restaurar editada  ·  ", base.fg(palette.overlay0)),
            Span::styled(" d ", base.fg(palette.red).add_modifier(Modifier::BOLD)),
            Span::styled("borrar de catálogo  ·  ", base.fg(palette.overlay0)),
            Span::styled(" Esc ", base.fg(palette.text).add_modifier(Modifier::BOLD)),
            Span::styled("cerrar", base.fg(palette.overlay0)),
        ]);
        Paragraph::new(footer_line).render(
            Rect::new(footer_area.x, footer_area.y + 1, footer_area.width, 1),
            buffer,
        );
    }

    let mut confirm_button = Rect::default();
    let mut cancel_button = Rect::default();

    // Render submodals
    match &state.sub_modal {
        SkillsSubModal::None => {}
        SkillsSubModal::Create(form) => {
            let (conf, canc) = render_create_skill_modal(buffer, inner, form, palette);
            confirm_button = conf;
            cancel_button = canc;
        }
        SkillsSubModal::ConfirmRestore {
            skill_name,
            message,
            ..
        } => {
            let (conf, canc) = render_skills_confirm_modal(
                buffer,
                inner,
                &format!("Confirmación: Restaurar skill '{skill_name}'"),
                message,
                " ↺ Restaurar skill ",
                palette.teal,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
        SkillsSubModal::ConfirmDelete {
            skill_name,
            scope,
            message,
        } => {
            let (conf, canc) = render_skills_confirm_modal(
                buffer,
                inner,
                &format!("Confirmación: Borrar skill '{skill_name}' ({scope})"),
                message,
                " 🗑 Borrar skill ",
                palette.red,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
    }

    Some(ShepherdSkillsRender {
        area: inner,
        close_button: Rect::default(),
        view_toggle_button: toggle_mode_btn,
        new_skill_button: new_skill_btn,
        restore_button: restore_btn,
        delete_skill_button: delete_btn,
        skill_rows,
        confirm_button,
        cancel_button,
    })
}

fn render_create_skill_modal(
    buffer: &mut Buffer,
    container: Rect,
    form: &CreateSkillFormState,
    palette: &Palette,
) -> (Rect, Rect) {
    let modal_width = 72.min(container.width.saturating_sub(4));
    let modal_height = 11.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_skills_box(buffer, modal_area, palette.panel_bg, palette.accent) {
        Some(r) => r,
        None => return (Rect::default(), Rect::default()),
    };

    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_stringn(
        inner.x + 1,
        inner.y,
        "Crear Nueva Skill en Catálogo",
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.text).add_modifier(Modifier::BOLD),
    );

    buffer.set_stringn(
        inner.x + 1,
        inner.y + 2,
        "Nombre (* minúsculas y guiones):",
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.overlay0),
    );

    let input_rect = Rect::new(inner.x + 1, inner.y + 3, inner.width.saturating_sub(2), 1);
    let input_style = Style::default().fg(palette.text).bg(palette.active_row_bg);
    for ix in input_rect.x..input_rect.right() {
        buffer[(ix, input_rect.y)]
            .set_symbol(" ")
            .set_style(input_style);
    }
    let display = format!("{}█", form.name);
    buffer.set_stringn(
        input_rect.x,
        input_rect.y,
        &display,
        usize::from(input_rect.width),
        input_style,
    );

    if let Some(ref err) = form.error {
        buffer.set_stringn(
            inner.x + 1,
            inner.y + 5,
            err,
            usize::from(inner.width.saturating_sub(2)),
            base.fg(palette.red).add_modifier(Modifier::BOLD),
        );
    }

    let btn_y = inner.bottom().saturating_sub(1);
    let confirm_btn = Rect::new(inner.x + 2, btn_y, 16, 1);
    let cancel_btn = Rect::new(inner.x + 20, btn_y, 16, 1);

    render_skills_btn(
        buffer,
        confirm_btn,
        " ↵ Crear skill ",
        palette.accent,
        palette,
    );
    render_skills_btn(
        buffer,
        cancel_btn,
        " esc Cancelar ",
        palette.surface_dim,
        palette,
    );

    (confirm_btn, cancel_btn)
}

fn render_skills_confirm_modal(
    buffer: &mut Buffer,
    container: Rect,
    title: &str,
    message: &str,
    confirm_label: &str,
    confirm_color: ratatui::style::Color,
    palette: &Palette,
) -> (Rect, Rect) {
    let modal_width = 76.min(container.width.saturating_sub(4));
    let modal_height = 9.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_skills_box(buffer, modal_area, palette.panel_bg, confirm_color) {
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

    buffer.set_stringn(
        inner.x + 1,
        inner.y + 2,
        message,
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.text),
    );

    let btn_y = inner.bottom().saturating_sub(1);
    let conf_w = (confirm_label.chars().count() as u16).max(16);
    let confirm_btn = Rect::new(inner.x + 2, btn_y, conf_w, 1);
    let cancel_btn = Rect::new(inner.x + 4 + conf_w, btn_y, 16, 1);

    render_skills_btn(buffer, confirm_btn, confirm_label, confirm_color, palette);
    render_skills_btn(
        buffer,
        cancel_btn,
        " esc Cancelar ",
        palette.surface_dim,
        palette,
    );

    (confirm_btn, cancel_btn)
}

fn render_skills_btn(
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

fn draw_skills_box(
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
    fn test_three_tiers_and_restart_notice_displayed_in_pure_render() {
        let skills = vec![
            AgentSkillItem {
                name: "conventional-commits".into(),
                description: "Format commits".into(),
                level: Level::Global,
                scope: Scope::Default,
                active: true,
                edited: false,
                shadowed_by: None,
            },
            AgentSkillItem {
                name: "implement-task".into(),
                description: "Focused tasks".into(),
                level: Level::Role,
                scope: Scope::Default,
                active: true,
                edited: true,
                shadowed_by: None,
            },
            AgentSkillItem {
                name: "my-custom-skill".into(),
                description: "Local custom".into(),
                level: Level::Session,
                scope: Scope::User,
                active: false,
                edited: false,
                shadowed_by: None,
            },
        ];

        let state = ShepherdSkillsState::new("coder-1", "coder", skills, Vec::new());
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let rendered = render_shepherd_skills_panel(&mut buffer, area, &state, &palette)
            .expect("should render skills panel");
        assert_eq!(rendered.skill_rows.len(), 3);

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("REINICIAR el agente"),
            "Must tell user changes apply on agent restart"
        );
        assert!(
            text.contains("sí llega en vivo"),
            "Must tell user text edit applies live"
        );
        assert!(text.contains("nivel:global"));
        assert!(text.contains("nivel:role"));
        assert!(text.contains("nivel:session"));
        assert!(
            text.contains("(editada)"),
            "Must indicate if skill was edited"
        );
    }

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
    fn test_skills_view_without_agent_shows_catalog_and_single_delete_button() {
        let catalog = vec![CatalogSkillItem {
            name: "git-review".into(),
            description: "Review pull requests".into(),
            scope: Scope::Default,
            is_global: false,
            enabled: true,
            problems: Vec::new(),
        }];
        let state = ShepherdSkillsState::new("", "", Vec::new(), catalog);
        assert_eq!(state.mode, SkillsViewMode::CatalogView);

        let area = Rect::new(0, 0, 92, 26);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let rendered = render_shepherd_skills_panel(&mut buffer, area, &state, &palette)
            .expect("should render skills panel");

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("Catálogo Completo"),
            "Must show catalog header without agent"
        );
        assert!(!text.contains("Agente ''"), "Must NOT show Agente ''");
        assert!(text.contains("git-review"), "Must show catalog skills");

        // "Borrar (d)" must appear exactly once in the action bar, and "Borrar (D)" must not appear
        let count_d = text.matches("Borrar (d)").count();
        let count_capital_d = text.matches("Borrar (D)").count();
        assert_eq!(
            count_d, 1,
            "Borrar (d) must appear exactly once in action bar"
        );
        assert_eq!(count_capital_d, 0, "Borrar (D) must NOT appear");

        // Action bar must fit within the area without being cut off
        assert!(
            rendered.delete_skill_button.right() <= area.width,
            "Action bar must fit without cutoff"
        );
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
