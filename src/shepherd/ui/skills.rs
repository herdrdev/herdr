use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
pub use crate::shepherd::skills::{
    AgentSkillItem, CatalogSkillItem, CreateSkillFormState, ShepherdSkillsAction, SkillsViewMode,
};
#[allow(unused_imports)]
pub use shepherd_core::skills::{Level, Scope};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdSkillsHits {
    pub list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub form_hits: Option<InlineFormHits>,
    pub stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdSkillsSectionState {
    pub mode: SkillsViewMode,
    pub selected: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub selected_agent_id: String,
    pub agent_role: String,
    pub agent_skills: Vec<AgentSkillItem>,
    pub catalog_skills: Vec<CatalogSkillItem>,
    pub form: Option<CreateSkillFormState>,
    pub confirm_restore: Option<String>,
    pub confirm_delete: Option<(String, Scope)>,
    pub last_action: Option<ShepherdSkillsAction>,
    pub error: Option<String>,
}

impl Default for ShepherdSkillsSectionState {
    fn default() -> Self {
        Self {
            mode: SkillsViewMode::CatalogView,
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            selected_agent_id: String::new(),
            agent_role: String::new(),
            agent_skills: Vec::new(),
            catalog_skills: Vec::new(),
            form: None,
            confirm_restore: None,
            confirm_delete: None,
            last_action: None,
            error: None,
        }
    }
}

impl ShepherdSkillsSectionState {
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
            mode,
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            selected_agent_id: agent_id.to_string(),
            agent_role: agent_role.to_string(),
            agent_skills,
            catalog_skills,
            form: None,
            confirm_restore: None,
            confirm_delete: None,
            last_action: None,
            error: None,
        }
    }

    pub fn has_sub_action(&self) -> bool {
        self.form.is_some() || self.confirm_restore.is_some() || self.confirm_delete.is_some()
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.form.is_some() || self.confirm_restore.is_some() || self.confirm_delete.is_some() {
            self.form = None;
            self.confirm_restore = None;
            self.confirm_delete = None;
            self.error = None;
            true
        } else {
            false
        }
    }

    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            SkillsViewMode::AgentView => SkillsViewMode::CatalogView,
            SkillsViewMode::CatalogView => SkillsViewMode::AgentView,
        };
        self.selected = 0;
        self.list_scroll = 0;
        self.detail_scroll = 0;
        self.cancel_sub_action();
    }

    pub fn selected_agent_skill(&self) -> Option<&AgentSkillItem> {
        self.agent_skills.get(self.selected)
    }

    pub fn selected_catalog_skill(&self) -> Option<&CatalogSkillItem> {
        self.catalog_skills.get(self.selected)
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
        let count = match self.mode {
            SkillsViewMode::AgentView => self.agent_skills.len(),
            SkillsViewMode::CatalogView => self.catalog_skills.len(),
        };
        if count > 0 && self.selected + 1 < count {
            self.selected += 1;
            self.detail_scroll = 0;
        }
    }

    pub fn start_create_skill(&mut self) {
        self.form = Some(CreateSkillFormState::default());
        self.confirm_restore = None;
        self.confirm_delete = None;
        self.error = None;
    }

    pub fn start_restore_skill(&mut self) {
        if self.mode == SkillsViewMode::AgentView {
            if let Some(skill) = self.selected_agent_skill() {
                if skill.edited {
                    self.confirm_restore = Some(skill.name.clone());
                    self.form = None;
                    self.confirm_delete = None;
                    self.error = None;
                }
            }
        }
    }

    pub fn start_delete_skill(&mut self) {
        if self.mode == SkillsViewMode::CatalogView {
            if let Some(skill) = self.selected_catalog_skill() {
                if skill.scope == Scope::Default {
                    self.error = Some(
                        "Skills bundled by default are protected and cannot be deleted".to_string(),
                    );
                } else {
                    self.confirm_delete = Some((skill.name.clone(), skill.scope));
                    self.form = None;
                    self.confirm_restore = None;
                    self.error = None;
                }
            }
        }
    }

    pub fn toggle_agent_skill(&mut self) -> Option<ShepherdSkillsAction> {
        if self.mode != SkillsViewMode::AgentView {
            return None;
        }
        let skill = self.selected_agent_skill()?.clone();
        if let Some(ref blocker) = skill.shadowed_by {
            self.error = Some(format!(
                "Cannot toggle '{}': it is shadowed by '{}'",
                skill.name, blocker
            ));
            return None;
        }
        let enable = !skill.active;
        if let Some(s) = self.agent_skills.get_mut(self.selected) {
            s.active = enable;
        }
        let action = ShepherdSkillsAction::ToggleAgentSkill {
            agent_id: self.selected_agent_id.clone(),
            skill_name: skill.name,
            enable,
        };
        self.last_action = Some(action.clone());
        Some(action)
    }

    pub fn toggle_catalog_global(&mut self) -> Option<ShepherdSkillsAction> {
        if self.mode != SkillsViewMode::CatalogView {
            return None;
        }
        let skill = self.selected_catalog_skill()?.clone();
        let make_global = !skill.is_global;
        if let Some(s) = self.catalog_skills.get_mut(self.selected) {
            s.is_global = make_global;
        }
        let action = ShepherdSkillsAction::ToggleCatalogGlobal {
            name: skill.name,
            make_global,
        };
        self.last_action = Some(action.clone());
        Some(action)
    }

    pub fn submit_create_skill(&mut self) -> Result<Option<ShepherdSkillsAction>, String> {
        let Some(ref form) = self.form else {
            return Ok(None);
        };
        match form.validate() {
            Ok(()) => {
                let action = ShepherdSkillsAction::CreateCatalogSkill {
                    name: form.name.trim().to_string(),
                    description: form.description.trim().to_string(),
                    scope: form.scope,
                    is_global: form.is_global,
                };
                self.last_action = Some(action.clone());
                self.form = None;
                Ok(Some(action))
            }
            Err(e) => {
                if let Some(ref mut f) = self.form {
                    f.error = Some(e.clone());
                }
                Err(e)
            }
        }
    }

    pub fn confirm_restore_action(&mut self) -> Option<ShepherdSkillsAction> {
        if let Some(skill_name) = self.confirm_restore.take() {
            let action = ShepherdSkillsAction::RestoreEditedSkill {
                agent_id: self.selected_agent_id.clone(),
                skill_name,
            };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn confirm_delete_action(&mut self) -> Option<ShepherdSkillsAction> {
        if let Some((name, scope)) = self.confirm_delete.take() {
            let action = ShepherdSkillsAction::DeleteCatalogSkill { name, scope };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn update_from_skills(&mut self, skills: &[shepherd_core::skills::Skill]) {
        self.catalog_skills = skills.iter().map(CatalogSkillItem::from).collect();
        if self.selected >= self.catalog_skills.len() {
            self.selected = self.catalog_skills.len().saturating_sub(1);
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdSkillsAction>) {
        // 1. Confirm restore active
        if self.confirm_restore.is_some() {
            match key.code {
                KeyCode::Enter => {
                    let act = self.confirm_restore_action();
                    return (true, act);
                }
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 2. Confirm delete active
        if self.confirm_delete.is_some() {
            match key.code {
                KeyCode::Enter => {
                    let act = self.confirm_delete_action();
                    return (true, act);
                }
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 3. Create skill form active
        if let Some(ref mut form) = self.form {
            match key.code {
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                KeyCode::Tab => {
                    // Cycle form fields / options
                    return (true, None);
                }
                KeyCode::Enter => match self.submit_create_skill() {
                    Ok(act) => return (true, act),
                    Err(_) => return (true, None),
                },
                KeyCode::Backspace => {
                    if !form.description.is_empty() {
                        form.description.pop();
                    } else if !form.name.is_empty() {
                        form.name.pop();
                    }
                    return (true, None);
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    if form.name.len() < 32 {
                        form.name.push(c);
                    } else {
                        form.description.push(c);
                    }
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 4. Normal view navigation and shortcuts
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                (true, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                (true, None)
            }
            KeyCode::Char('c') => {
                self.toggle_mode();
                (true, None)
            }
            KeyCode::Char(' ') => {
                let act = self.toggle_agent_skill();
                (true, act)
            }
            KeyCode::Char('n') if self.mode == SkillsViewMode::CatalogView => {
                self.start_create_skill();
                (true, None)
            }
            KeyCode::Char('g') if self.mode == SkillsViewMode::CatalogView => {
                let act = self.toggle_catalog_global();
                (true, act)
            }
            KeyCode::Char('r') if self.mode == SkillsViewMode::AgentView => {
                self.start_restore_skill();
                (true, None)
            }
            KeyCode::Char('d') | KeyCode::Char('D') if self.mode == SkillsViewMode::CatalogView => {
                self.start_delete_skill();
                (true, None)
            }
            _ => (false, None),
        }
    }
}

pub fn render_skills_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdSkillsSectionState,
    palette: &Palette,
) -> ShepherdSkillsHits {
    let mut hits = ShepherdSkillsHits::default();
    if area.width < 10 || area.height < 4 {
        return hits;
    }

    let wide_list_w = (area.width * 34 / 100).max(24);

    // Filter tabs for mode
    let mode_tabs: [&str; 2] = ["By Agent", "Catalog"];
    let active_mode_idx = match state.mode {
        SkillsViewMode::AgentView => 0,
        SkillsViewMode::CatalogView => 1,
    };

    let action_button = if state.mode == SkillsViewMode::CatalogView {
        Some(("+ New", "n"))
    } else {
        None
    };

    let items: Vec<ListViewItem> = match state.mode {
        SkillsViewMode::AgentView => state
            .agent_skills
            .iter()
            .map(|s| {
                let check = if s.active { "[✓]" } else { "[ ]" };
                let scope_str = format!("[{:?}]", s.scope).to_lowercase();
                let scope_style = match s.scope {
                    Scope::Default => Style::default().fg(palette.overlay0),
                    Scope::User => Style::default().fg(palette.accent),
                    Scope::Repo => Style::default().fg(palette.green),
                };
                let meta = if s.edited {
                    Some("[edited]".to_string())
                } else if s.shadowed_by.is_some() {
                    Some("[shadowed]".to_string())
                } else {
                    None
                };
                ListViewItem {
                    title: format!("{check} {}", s.name),
                    badge: Some((scope_str, scope_style)),
                    meta,
                }
            })
            .collect(),
        SkillsViewMode::CatalogView => state
            .catalog_skills
            .iter()
            .map(|s| {
                let scope_str = format!("[{:?}]", s.scope).to_lowercase();
                let scope_style = match s.scope {
                    Scope::Default => Style::default().fg(palette.overlay0),
                    Scope::User => Style::default().fg(palette.accent),
                    Scope::Repo => Style::default().fg(palette.green),
                };
                let meta = if s.is_global {
                    Some("global".to_string())
                } else {
                    None
                };
                ListViewItem {
                    title: s.name.clone(),
                    badge: Some((scope_str, scope_style)),
                    meta,
                }
            })
            .collect(),
    };

    let split = split_panes(
        area,
        wide_list_w,
        compact_panel(state.has_sub_action(), items.is_empty()),
    );
    let list_area = split.list;
    hits.list_hits = render_list_view(
        buffer,
        list_area,
        &items,
        state.selected,
        state.list_scroll,
        Some((&mode_tabs, active_mode_idx)),
        action_button,
        palette,
    );

    let Some(detail_area) = split.detail else {
        return hits;
    };
    if let Some(div_x) = split.divider_x {
        draw_divider(
            buffer,
            area,
            div_x,
            Style::default().fg(palette.surface_dim),
        );
    }

    // 1. Inline form for creating catalog skill
    if let Some(ref form) = state.form {
        let inline_state = InlineFormState {
            title: "NEW SKILL (inline form)".to_string(),
            fields: vec![
                FormField {
                    label: "Skill name".to_string(),
                    value: form.name.clone(),
                    placeholder: "ej. rust-tools".to_string(),
                },
                FormField {
                    label: "Description".to_string(),
                    value: form.description.clone(),
                    placeholder: "Operating instructions for the agent...".to_string(),
                },
                FormField {
                    label: "Scope".to_string(),
                    value: format!("{:?}", form.scope).to_lowercase(),
                    placeholder: "user or repo".to_string(),
                },
                FormField {
                    label: "Global (y/n)".to_string(),
                    value: if form.is_global { "y" } else { "n" }.to_string(),
                    placeholder: "y = active for all agents".to_string(),
                },
            ],
            active_field: 0,
            error: form.error.clone(),
            success: None,
        };
        hits.form_hits = Some(render_inline_form(
            buffer,
            detail_area,
            &inline_state,
            "Save Skill",
            "Cancel",
            true,
            palette,
        ));
        return hits;
    }

    // 2. Confirm restore stripe
    if let Some(ref skill_name) = state.confirm_restore {
        let msg = format!(
            "Restore skill '{skill_name}' to its original content? Local changes will be lost."
        );
        hits.stripe_hits = Some(render_confirm_stripe(
            buffer,
            detail_area,
            &msg,
            "Restore",
            "Cancel",
            false,
            palette,
        ));
        return hits;
    }

    // 3. Confirm delete stripe
    if let Some((ref skill_name, scope)) = state.confirm_delete {
        let msg = format!(
            "Delete skill '{skill_name}' with scope {:?} from the catalog? This cannot be undone.",
            scope
        );
        hits.stripe_hits = Some(render_confirm_stripe(
            buffer,
            detail_area,
            &msg,
            "Delete",
            "Cancel",
            true,
            palette,
        ));
        return hits;
    }

    // 4. Empty state
    let is_empty = match state.mode {
        SkillsViewMode::AgentView => state.agent_skills.is_empty(),
        SkillsViewMode::CatalogView => state.catalog_skills.is_empty(),
    };

    if is_empty {
        let (title, desc, actions): (&str, &str, &[(&str, Option<&str>)]) = match state.mode {
            SkillsViewMode::AgentView => (
                "NO SKILLS ASSIGNED",
                "The selected agent has no specific skills assigned.\nSwitch to the Catalog to browse or create skills.",
                &[("View Catalog", Some("c"))],
            ),
            SkillsViewMode::CatalogView => (
                "SKILL CATALOG EMPTY",
                "No skills registered in the catalog.\nYou can create a new custom skill.",
                &[("+ New Skill", Some("n"))],
            ),
        };
        hits.empty_hits = Some(render_empty_state(
            buffer,
            detail_area,
            title,
            desc,
            actions,
            palette,
        ));
        return hits;
    }

    // 5. Normal Detail view
    match state.mode {
        SkillsViewMode::AgentView => {
            if let Some(skill) = state.selected_agent_skill() {
                let scope_style = match skill.scope {
                    Scope::Default => Style::default().fg(palette.overlay0),
                    Scope::User => Style::default().fg(palette.accent),
                    Scope::Repo => Style::default().fg(palette.green),
                };
                let status_style = if skill.active {
                    Style::default()
                        .fg(palette.green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0)
                };
                let mut badges = vec![
                    (format!("[{:?}]", skill.scope).to_lowercase(), scope_style),
                    (
                        if skill.active {
                            "[active]".to_string()
                        } else {
                            "[inactiva]".to_string()
                        },
                        status_style,
                    ),
                ];
                if skill.edited {
                    badges.push((
                        "[modificada]".to_string(),
                        Style::default().fg(palette.yellow),
                    ));
                }

                let meta_line = format!(
                    "Agent: {} ({}) | Scope: {:?} | Modified: {}",
                    if state.selected_agent_id.is_empty() {
                        "all"
                    } else {
                        &state.selected_agent_id
                    },
                    if state.agent_role.is_empty() {
                        "general"
                    } else {
                        &state.agent_role
                    },
                    skill.scope,
                    if skill.edited { "Yes" } else { "No" }
                );

                let header = DetailHeader {
                    title: format!("SKILL: {}", skill.name),
                    badges,
                    metadata_line: Some(meta_line),
                    actions: vec![("Alternar".to_string(), Some("Space".into()))],
                };

                let mut blocks = vec![
                    DetailContentBlock::Heading("DESCRIPTION:".into()),
                    DetailContentBlock::Paragraph(skill.description.clone()),
                    DetailContentBlock::Notice {
                        text: "NOTICE:\n• Adding or removing skills takes effect when the agent is RESTARTED.\n• Editing a skill's text propagates LIVE immediately.".into(),
                        level: AlertLevel::Info,
                    },
                ];

                if let Some(ref blocker) = skill.shadowed_by {
                    blocks.push(DetailContentBlock::Notice {
                        text: format!("SHADOWING NOTICE:\nThis setting is shadowed by the upper layer: '{blocker}'."),
                        level: AlertLevel::Warning,
                    });
                }

                let mut bottom_actions = vec![("Toggle Activation", "Space", true)];
                if skill.edited {
                    bottom_actions.push(("Restore Original Copy", "r", false));
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
        }
        SkillsViewMode::CatalogView => {
            if let Some(skill) = state.selected_catalog_skill() {
                let scope_style = match skill.scope {
                    Scope::Default => Style::default().fg(palette.overlay0),
                    Scope::User => Style::default().fg(palette.accent),
                    Scope::Repo => Style::default().fg(palette.green),
                };
                let mut badges = vec![(format!("[{:?}]", skill.scope).to_lowercase(), scope_style)];
                if skill.is_global {
                    badges.push(("[global]".to_string(), Style::default().fg(palette.accent)));
                }
                if !skill.enabled {
                    badges.push((
                        "[with errors]".to_string(),
                        Style::default().fg(palette.red),
                    ));
                }

                let meta_line = format!(
                    "Scope: {:?} | Global: {} | Enabled: {}",
                    skill.scope,
                    if skill.is_global { "Yes" } else { "No" },
                    if skill.enabled { "Yes" } else { "No" }
                );

                let header = DetailHeader {
                    title: format!("SKILL: {}", skill.name),
                    badges,
                    metadata_line: Some(meta_line),
                    actions: vec![("+ New".to_string(), Some("n".into()))],
                };

                let mut blocks = vec![
                    DetailContentBlock::Heading("DESCRIPTION:".into()),
                    DetailContentBlock::Paragraph(skill.description.clone()),
                ];

                if !skill.problems.is_empty() {
                    blocks.push(DetailContentBlock::Notice {
                        text: format!("Problemas detectados:\n{}", skill.problems.join("\n")),
                        level: AlertLevel::Warning,
                    });
                }

                let mut bottom_actions = vec![("Alternar Global", "g", true)];
                if skill.scope != Scope::Default {
                    bottom_actions.push(("Delete from Catalog", "d", false));
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
        }
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
    fn test_skills_section_pure_render_and_empty_state() {
        let state = ShepherdSkillsSectionState::default();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        let area = buffer.area;
        let hits = render_skills_section(&mut buffer, area, &state, &test_palette());
        assert!(hits.empty_hits.is_some());
        assert!(!hits.list_hits.filter_tabs.is_empty());
    }

    #[test]
    fn test_skills_section_navigation_and_mode_toggle() {
        let mut state = ShepherdSkillsSectionState::default();
        state.catalog_skills = vec![
            CatalogSkillItem {
                name: "skill-a".into(),
                description: "Skill A".into(),
                scope: Scope::Default,
                is_global: false,
                enabled: true,
                problems: Vec::new(),
            },
            CatalogSkillItem {
                name: "skill-b".into(),
                description: "Skill B".into(),
                scope: Scope::User,
                is_global: true,
                enabled: true,
                problems: Vec::new(),
            },
        ];
        assert_eq!(state.selected, 0);
        state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.selected, 1);
        state.handle_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.selected, 0);

        // Toggle mode
        state.handle_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(state.mode, SkillsViewMode::AgentView);
    }

    #[test]
    fn test_skills_section_destructive_guard_delete_skill() {
        let mut state = ShepherdSkillsSectionState::default();
        state.catalog_skills = vec![
            CatalogSkillItem {
                name: "builtin-safe".into(),
                description: "Builtin".into(),
                scope: Scope::Default,
                is_global: false,
                enabled: true,
                problems: Vec::new(),
            },
            CatalogSkillItem {
                name: "user-tool".into(),
                description: "User tool".into(),
                scope: Scope::User,
                is_global: false,
                enabled: true,
                problems: Vec::new(),
            },
        ];

        // 1. Trying to delete default skill: blocked with error, no confirm stripe
        state.selected = 0;
        state.handle_key(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(state.confirm_delete.is_none());
        assert!(state.error.is_some());

        // 2. Trying to delete user skill: opens confirm stripe
        state.selected = 1;
        state.handle_key(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(
            state.confirm_delete,
            Some(("user-tool".to_string(), Scope::User))
        );
        assert!(state.has_sub_action());

        // 3. Esc cancels confirm stripe without deleting
        state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.confirm_delete.is_none());
        assert_eq!(state.last_action, None);

        // 4. Trigger delete and Enter confirms
        state.handle_key(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        let (_, act) = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            act,
            Some(ShepherdSkillsAction::DeleteCatalogSkill {
                name: "user-tool".into(),
                scope: Scope::User,
            })
        );
    }

    #[test]
    fn test_skills_section_destructive_guard_restore_skill() {
        let mut state = ShepherdSkillsSectionState::new(
            "coder-1",
            "coder",
            vec![AgentSkillItem {
                name: "git-safe".into(),
                description: "Safe git".into(),
                level: Level::Role,
                scope: Scope::Default,
                active: true,
                edited: true,
                shadowed_by: None,
            }],
            Vec::new(),
        );

        // Press 'r': requires confirmation
        state.handle_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(state.confirm_restore, Some("git-safe".into()));
        assert!(state.has_sub_action());

        // Esc cancels
        state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(state.confirm_restore, None);

        // Press 'r' then Enter confirms
        state.handle_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        let (_, act) = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            act,
            Some(ShepherdSkillsAction::RestoreEditedSkill {
                agent_id: "coder-1".into(),
                skill_name: "git-safe".into(),
            })
        );
    }
}
