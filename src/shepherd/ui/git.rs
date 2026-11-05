use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use std::path::PathBuf;

use crate::app::state::Palette;
use super::components::{
    confirm_stripe::{render_confirm_stripe, ConfirmStripeHits},
    detail_view::{render_detail_view, AlertLevel, DetailContentBlock, DetailHeader, DetailViewHits},
    empty_state::{render_empty_state, EmptyStateHits},
    inline_form::{render_inline_form, FormField, InlineFormHits, InlineFormState},
    list_view::{render_list_view, ListViewHits, ListViewItem},
};
pub use crate::shepherd::git::{GitRepoStatus, ShepherdGitAction, ShepherdGitRow};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdGitHits {
    pub list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub form_hits: Option<InlineFormHits>,
    pub stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdGitSectionState {
    pub selected: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub rows: Vec<ShepherdGitRow>,
    pub gh_available: bool,
    pub gh_note: Option<String>,
    pub error: Option<String>,
    pub worktree_form: Option<InlineFormState>,
    pub branch_form: Option<InlineFormState>,
    pub confirm_publish: Option<String>,
    pub confirm_remove: Option<String>,
    pub last_action: Option<ShepherdGitAction>,
}

impl Default for ShepherdGitSectionState {
    fn default() -> Self {
        Self {
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            rows: Vec::new(),
            gh_available: true,
            gh_note: None,
            error: None,
            worktree_form: None,
            branch_form: None,
            confirm_publish: None,
            confirm_remove: None,
            last_action: None,
        }
    }
}

impl ShepherdGitSectionState {
    pub fn new(rows: Vec<ShepherdGitRow>) -> Self {
        Self {
            selected: 0,
            list_scroll: 0,
            detail_scroll: 0,
            rows,
            gh_available: true,
            gh_note: None,
            error: None,
            worktree_form: None,
            branch_form: None,
            confirm_publish: None,
            confirm_remove: None,
            last_action: None,
        }
    }

    pub fn has_sub_action(&self) -> bool {
        self.worktree_form.is_some()
            || self.branch_form.is_some()
            || self.confirm_publish.is_some()
            || self.confirm_remove.is_some()
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.has_sub_action() {
            self.worktree_form = None;
            self.branch_form = None;
            self.confirm_publish = None;
            self.confirm_remove = None;
            self.error = None;
            true
        } else {
            false
        }
    }

    pub fn selected_row(&self) -> Option<&ShepherdGitRow> {
        self.rows.get(self.selected)
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
        if !self.rows.is_empty() && self.selected + 1 < self.rows.len() {
            self.selected += 1;
            self.detail_scroll = 0;
        }
    }

    pub fn start_create_worktree(&mut self) {
        self.worktree_form = Some(InlineFormState::new(
            "NUEVO WORKTREE (Formulario en línea)",
            vec![
                FormField::new("Ruta del nuevo worktree", "ej. .shepherd/worktrees/task-12"),
                FormField::new("Rama o commit base", "ej. master o HEAD"),
            ],
        ));
        self.branch_form = None;
        self.confirm_publish = None;
        self.confirm_remove = None;
        self.error = None;
    }

    pub fn start_create_branch(&mut self) {
        self.branch_form = Some(InlineFormState::new(
            "NUEVA RAMA (Formulario en línea)",
            vec![
                FormField::new("Nombre de la nueva rama", "ej. feature/parser-flags"),
                FormField::new("Punto de partida (base)", "ej. master"),
            ],
        ));
        self.worktree_form = None;
        self.confirm_publish = None;
        self.confirm_remove = None;
        self.error = None;
    }

    pub fn start_publish_branch(&mut self) {
        if let Some(row) = self.selected_row() {
            self.confirm_publish = Some(row.branch.clone());
            self.confirm_remove = None;
            self.worktree_form = None;
            self.branch_form = None;
            self.error = None;
        }
    }

    pub fn start_remove_worktree(&mut self) {
        if let Some(row) = self.selected_row() {
            if row.is_main {
                self.error = Some("El worktree principal no se puede eliminar".into());
            } else {
                self.confirm_remove = Some(row.path.clone());
                self.confirm_publish = None;
                self.worktree_form = None;
                self.branch_form = None;
                self.error = None;
            }
        }
    }

    pub fn submit_create_worktree(&mut self) -> Result<Option<ShepherdGitAction>, String> {
        let Some(ref form) = self.worktree_form else {
            return Ok(None);
        };
        let path = form.fields[0].value.trim().to_string();
        let branch = form.fields[1].value.trim().to_string();
        if path.is_empty() {
            return Err("La ruta del worktree es obligatoria".into());
        }
        let action = ShepherdGitAction::CreateWorktree { path, branch };
        self.last_action = Some(action.clone());
        self.worktree_form = None;
        Ok(Some(action))
    }

    pub fn submit_create_branch(&mut self) -> Result<Option<ShepherdGitAction>, String> {
        let Some(ref form) = self.branch_form else {
            return Ok(None);
        };
        let branch = form.fields[0].value.trim().to_string();
        let base = form.fields[1].value.trim().to_string();
        if branch.is_empty() {
            return Err("El nombre de la rama es obligatorio".into());
        }
        let action = ShepherdGitAction::CreateBranch { branch, base };
        self.last_action = Some(action.clone());
        self.branch_form = None;
        Ok(Some(action))
    }

    pub fn confirm_publish_action(&mut self) -> Option<ShepherdGitAction> {
        if let Some(branch) = self.confirm_publish.take() {
            let action = ShepherdGitAction::PublishBranch { branch };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn confirm_remove_action(&mut self) -> Option<ShepherdGitAction> {
        if let Some(path) = self.confirm_remove.take() {
            let action = ShepherdGitAction::RemoveWorktree { path };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn update_from_status(&mut self, status: &GitRepoStatus) {
        self.rows = status
            .worktrees
            .iter()
            .map(|wt| ShepherdGitRow::from_worktree_info(wt, status.gh_available))
            .collect();
        self.gh_available = status.gh_available;
        self.gh_note = status.gh_note.clone();
        self.error = status.error.clone();
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdGitAction>) {
        // 1. Confirm publish active
        if self.confirm_publish.is_some() {
            match key.code {
                KeyCode::Enter => {
                    let act = self.confirm_publish_action();
                    return (true, act);
                }
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 2. Confirm remove active
        if self.confirm_remove.is_some() {
            match key.code {
                KeyCode::Enter => {
                    let act = self.confirm_remove_action();
                    return (true, act);
                }
                KeyCode::Esc => {
                    self.cancel_sub_action();
                    return (true, None);
                }
                _ => return (true, None),
            }
        }

        // 3. Worktree form active
        if let Some(ref mut form) = self.worktree_form {
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
                    match self.submit_create_worktree() {
                        Ok(act) => return (true, act),
                        Err(e) => {
                            if let Some(ref mut f) = self.worktree_form {
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

        // 4. Branch form active
        if let Some(ref mut form) = self.branch_form {
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
                    match self.submit_create_branch() {
                        Ok(act) => return (true, act),
                        Err(e) => {
                            if let Some(ref mut f) = self.branch_form {
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
            KeyCode::Char('w') => {
                self.start_create_worktree();
                (true, None)
            }
            KeyCode::Char('b') => {
                self.start_create_branch();
                (true, None)
            }
            KeyCode::Char('p') => {
                self.start_publish_branch();
                (true, None)
            }
            KeyCode::Char('x') => {
                self.start_remove_worktree();
                (true, None)
            }
            KeyCode::Char('r') => {
                let action = ShepherdGitAction::Refresh;
                self.last_action = Some(action.clone());
                (true, Some(action))
            }
            KeyCode::Char('c') => {
                if let Some(row) = self.selected_row() {
                    let action = ShepherdGitAction::CopyOrShowPath {
                        path: PathBuf::from(&row.path),
                    };
                    self.last_action = Some(action.clone());
                    (true, Some(action))
                } else {
                    (true, None)
                }
            }
            _ => (false, None),
        }
    }
}

pub fn render_git_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdGitSectionState,
    palette: &Palette,
) -> ShepherdGitHits {
    let mut hits = ShepherdGitHits::default();
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
        .rows
        .iter()
        .map(|r| {
            let badge_str = if r.is_clean {
                "[clean]".to_string()
            } else {
                format!("[dirty {}]", r.dirty_count)
            };
            let badge_style = if r.is_clean {
                Style::default().fg(palette.green)
            } else {
                Style::default().fg(palette.yellow)
            };
            let meta = if r.ahead > 0 || r.behind > 0 {
                Some(format!("+{} -{}", r.ahead, r.behind))
            } else if r.is_main {
                Some("main".into())
            } else {
                None
            };
            ListViewItem {
                title: r.branch.clone(),
                badge: Some((badge_str, badge_style)),
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
        Some(("+ Worktree", "w")),
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

    // 1. Worktree inline form
    if let Some(ref form) = state.worktree_form {
        hits.form_hits = Some(render_inline_form(
            buffer,
            detail_area,
            form,
            "Crear Worktree",
            "Cancelar",
            true,
            palette,
        ));
        return hits;
    }

    // 2. Branch inline form
    if let Some(ref form) = state.branch_form {
        hits.form_hits = Some(render_inline_form(
            buffer,
            detail_area,
            form,
            "Crear Rama",
            "Cancelar",
            true,
            palette,
        ));
        return hits;
    }

    // 3. Confirm publish stripe
    if let Some(ref branch) = state.confirm_publish {
        let msg = format!("¿Publicar la rama '{branch}' en el repositorio remoto?");
        hits.stripe_hits = Some(render_confirm_stripe(
            buffer,
            detail_area,
            &msg,
            "Publicar Rama",
            "Cancelar",
            false,
            palette,
        ));
        return hits;
    }

    // 4. Confirm remove stripe
    if let Some(ref path) = state.confirm_remove {
        let msg = format!("¿Eliminar el worktree '{path}' del disco? Se perderán cambios no guardados.");
        hits.stripe_hits = Some(render_confirm_stripe(
            buffer,
            detail_area,
            &msg,
            "Eliminar Worktree",
            "Cancelar",
            true,
            palette,
        ));
        return hits;
    }

    // 5. Empty state
    if state.rows.is_empty() {
        hits.empty_hits = Some(render_empty_state(
            buffer,
            detail_area,
            "NO HAY WORKTREES REGISTRADOS",
            "No se encontraron worktrees ni ramas activas en el repositorio.\nPuedes registrar un nuevo worktree para un agente.",
            &[("+ Nuevo Worktree", Some("w"))],
            palette,
        ));
        return hits;
    }

    // 6. Normal Detail view
    if let Some(row) = state.selected_row() {
        let mut badges = Vec::new();
        if row.is_main {
            badges.push(("[main]".to_string(), Style::default().fg(palette.accent)));
        }
        if row.is_clean {
            badges.push(("[clean]".to_string(), Style::default().fg(palette.green)));
        } else {
            badges.push((
                format!("[dirty {}]", row.dirty_count),
                Style::default().fg(palette.yellow),
            ));
        }

        let meta_line = format!(
            "Rama: {} | Ahead: {} | Behind: {} | Agente: {}",
            row.branch,
            row.ahead,
            row.behind,
            row.agent_id.as_deref().unwrap_or("ninguno")
        );

        let header = DetailHeader {
            title: format!("WORKTREE: {}", row.path),
            badges,
            metadata_line: Some(meta_line),
            actions: vec![
                ("+ Worktree".to_string(), Some("w".into())),
                ("+ Rama".to_string(), Some("b".into())),
            ],
        };

        let mut blocks = vec![
            DetailContentBlock::Heading("ESTADO DEL WORKTREE:".into()),
            DetailContentBlock::Paragraph(format!(
                "Directorio: {}\nRama activa: {}\nUpstream: {}",
                row.path,
                row.branch,
                row.upstream.as_deref().unwrap_or("ninguno configurado")
            )),
        ];

        if row.pr_display != "—" {
            blocks.push(DetailContentBlock::Notice {
                text: format!(
                    "PULL REQUEST ASOCIADO:\n{} ({})",
                    row.pr_display,
                    row.pr_url.as_deref().unwrap_or("sin url")
                ),
                level: AlertLevel::Info,
            });
        }

        if let Some(ref agent_id) = row.agent_id {
            blocks.push(DetailContentBlock::Notice {
                text: format!("AGENTE VINCULADO:\nAgente activo '{agent_id}' asignado a este worktree."),
                level: AlertLevel::Info,
            });
        }

        let mut bottom_actions = vec![
            ("Nueva Rama", "b", true),
            ("Publicar Rama", "p", false),
            ("Copiar Ruta", "c", false),
            ("Refrescar", "r", false),
        ];

        if !row.is_main {
            bottom_actions.push(("Eliminar Worktree", "x", false));
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
    fn test_git_section_pure_render_and_empty_state() {
        let state = ShepherdGitSectionState::default();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        let area = buffer.area;
        let hits = render_git_section(&mut buffer, area, &state, &test_palette());
        assert!(hits.empty_hits.is_some());
        assert!(hits.list_hits.action_button.is_some());
    }

    #[test]
    fn test_git_section_navigation_and_selection() {
        let mut state = ShepherdGitSectionState::new(vec![
            ShepherdGitRow {
                path: "/repo".into(),
                branch: "master".into(),
                is_main: true,
                is_clean: true,
                dirty_count: 0,
                ahead: 0,
                behind: 0,
                upstream: None,
                agent_id: None,
                pr_display: "—".into(),
                pr_url: None,
            },
            ShepherdGitRow {
                path: "/repo/.shepherd/worktrees/task-1".into(),
                branch: "feature/1".into(),
                is_main: false,
                is_clean: false,
                dirty_count: 2,
                ahead: 1,
                behind: 0,
                upstream: None,
                agent_id: Some("coder-1".into()),
                pr_display: "#12 open".into(),
                pr_url: Some("https://github.com/herdr/herdr/pull/12".into()),
            },
        ]);

        assert_eq!(state.selected, 0);
        state.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_row().unwrap().branch, "feature/1");
    }

    #[test]
    fn test_git_section_destructive_guards() {
        let mut state = ShepherdGitSectionState::new(vec![
            ShepherdGitRow {
                path: "/repo".into(),
                branch: "master".into(),
                is_main: true,
                is_clean: true,
                dirty_count: 0,
                ahead: 0,
                behind: 0,
                upstream: None,
                agent_id: None,
                pr_display: "—".into(),
                pr_url: None,
            },
            ShepherdGitRow {
                path: "/repo/.shepherd/worktrees/task-1".into(),
                branch: "feature/1".into(),
                is_main: false,
                is_clean: false,
                dirty_count: 2,
                ahead: 1,
                behind: 0,
                upstream: None,
                agent_id: None,
                pr_display: "—".into(),
                pr_url: None,
            },
        ]);

        // 1. Trying to remove main worktree: blocked
        state.selected = 0;
        state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(state.confirm_remove.is_none());
        assert!(state.error.is_some());

        // 2. Removing feature worktree: opens confirm stripe
        state.selected = 1;
        state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(
            state.confirm_remove,
            Some("/repo/.shepherd/worktrees/task-1".into())
        );
        assert!(state.has_sub_action());

        // 3. Esc cancels
        state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.confirm_remove.is_none());

        // 4. Enter confirms remove
        state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let (_, act) = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            act,
            Some(ShepherdGitAction::RemoveWorktree {
                path: "/repo/.shepherd/worktrees/task-1".into(),
            })
        );
    }
}
