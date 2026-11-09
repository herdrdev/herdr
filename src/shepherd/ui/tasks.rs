use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::components::{
    compact_panel,
    confirm_stripe::{render_confirm_stripe, ConfirmStripeHits},
    detail_view::{render_detail_view, DetailContentBlock, DetailHeader, DetailViewHits},
    draw_divider,
    empty_state::{render_empty_state, EmptyStateHits},
    inline_form::{render_inline_form, FormField, InlineFormHits, InlineFormState},
    list_view::{render_list_view, ListViewHits, ListViewItem},
    split_panes,
};
use crate::app::state::Palette;
pub use crate::shepherd::tasks::{
    CreateTaskFormState, ImportGhFormState, ShepherdTaskRow, ShepherdTasksAction,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdTasksHits {
    pub list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub form_hits: Option<InlineFormHits>,
    pub stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdTasksSectionState {
    pub selected: usize,
    pub filter_index: usize, // 0: Todas, 1: Abiertas, 2: Completadas
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub tasks: Vec<ShepherdTaskRow>,
    pub form: Option<CreateTaskFormState>,
    pub import_gh: Option<ImportGhFormState>,
    pub confirm_archive: Option<u32>,
    pub last_action: Option<ShepherdTasksAction>,
    pub error: Option<String>,
}

impl Default for ShepherdTasksSectionState {
    fn default() -> Self {
        Self {
            selected: 0,
            filter_index: 0,
            list_scroll: 0,
            detail_scroll: 0,
            tasks: Vec::new(),
            form: None,
            import_gh: None,
            confirm_archive: None,
            last_action: None,
            error: None,
        }
    }
}

impl ShepherdTasksSectionState {
    pub fn filtered_indices(&self) -> Vec<usize> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| match self.filter_index {
                1 => t.state.to_lowercase() == "open" || t.state.to_lowercase() == "active",
                2 => {
                    t.state.to_lowercase() == "done"
                        || t.state.to_lowercase() == "completed"
                        || t.state.to_lowercase() == "archived"
                }
                _ => true,
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_task(&self) -> Option<&ShepherdTaskRow> {
        let indices = self.filtered_indices();
        indices
            .get(self.selected)
            .and_then(|&idx| self.tasks.get(idx))
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
        let count = self.filtered_indices().len();
        if count > 0 && self.selected + 1 < count {
            self.selected += 1;
            self.detail_scroll = 0;
        }
    }

    pub fn start_create_task(&mut self) {
        self.form = Some(CreateTaskFormState::default());
        self.import_gh = None;
        self.confirm_archive = None;
        self.error = None;
    }

    pub fn start_import_gh(&mut self) {
        self.import_gh = Some(ImportGhFormState::default());
        self.form = None;
        self.confirm_archive = None;
        self.error = None;
    }

    pub fn start_archive_task(&mut self) {
        if let Some(task) = self.selected_task() {
            self.confirm_archive = Some(task.id);
            self.form = None;
            self.import_gh = None;
            self.error = None;
        }
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.form.is_some() || self.import_gh.is_some() || self.confirm_archive.is_some() {
            self.form = None;
            self.import_gh = None;
            self.confirm_archive = None;
            self.error = None;
            true
        } else {
            false
        }
    }

    pub fn submit_create_task(&mut self) -> Result<Option<ShepherdTasksAction>, String> {
        let Some(ref form) = self.form else {
            return Ok(None);
        };
        match form.validate() {
            Ok((criteria, attachments)) => {
                let action = ShepherdTasksAction::CreateTask {
                    title: form.title.trim().to_string(),
                    description: form.description.trim().to_string(),
                    criteria,
                    attachments,
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

    pub fn submit_import_gh(&mut self) -> Result<Option<ShepherdTasksAction>, String> {
        let Some(ref import_form) = self.import_gh else {
            return Ok(None);
        };
        match import_form.validate() {
            Ok(issue_ref) => {
                let action = ShepherdTasksAction::ImportGhIssue { issue_ref };
                self.last_action = Some(action.clone());
                self.import_gh = None;
                Ok(Some(action))
            }
            Err(e) => {
                if let Some(ref mut f) = self.import_gh {
                    f.error = Some(e.clone());
                }
                Err(e)
            }
        }
    }

    pub fn confirm_archive_task(&mut self) -> Option<ShepherdTasksAction> {
        if let Some(task_id) = self.confirm_archive.take() {
            let action = ShepherdTasksAction::ArchiveTask { task_id };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn open_editor_action(&mut self) -> Option<ShepherdTasksAction> {
        if let Some(task) = self.selected_task() {
            let spec_path = task.directory.join("spec.md");
            let file_path = if spec_path.exists() {
                spec_path
            } else {
                task.directory.join("issue.md")
            };
            let action = ShepherdTasksAction::OpenEditor {
                task_id: task.id,
                file_path,
            };
            self.last_action = Some(action.clone());
            Some(action)
        } else {
            None
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdTasksAction>) {
        // 1. If create task form is open
        if let Some(ref mut form) = self.form {
            match key.code {
                KeyCode::Esc => {
                    self.form = None;
                    return (true, None);
                }
                KeyCode::Tab => {
                    form.cycle_field_forward();
                    return (true, None);
                }
                KeyCode::BackTab => {
                    form.cycle_field_backward();
                    return (true, None);
                }
                KeyCode::Enter => match self.submit_create_task() {
                    Ok(act) => return (true, act),
                    Err(_) => return (true, None),
                },
                KeyCode::Backspace => {
                    form.delete_char();
                    return (true, None);
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    form.insert_char(c);
                    return (true, None);
                }
                _ => return (false, None),
            }
        }

        // 2. If import GitHub form is open
        if let Some(ref mut import_form) = self.import_gh {
            match key.code {
                KeyCode::Esc => {
                    self.import_gh = None;
                    return (true, None);
                }
                KeyCode::Enter => match self.submit_import_gh() {
                    Ok(act) => return (true, act),
                    Err(_) => return (true, None),
                },
                KeyCode::Backspace => {
                    import_form.delete_char();
                    return (true, None);
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    import_form.insert_char(c);
                    return (true, None);
                }
                _ => return (false, None),
            }
        }

        // 3. If archive confirmation stripe is active
        if self.confirm_archive.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('c') => {
                    self.confirm_archive = None;
                    return (true, None);
                }
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('a') => {
                    let act = self.confirm_archive_task();
                    return (true, act);
                }
                _ => return (false, None),
            }
        }

        // 4. Normal list & navigation keys
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                (true, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                (true, None)
            }
            KeyCode::Char('n') => {
                self.start_create_task();
                (true, None)
            }
            KeyCode::Char('i') => {
                self.start_import_gh();
                (true, None)
            }
            KeyCode::Char('a') => {
                self.start_archive_task();
                (true, None)
            }
            KeyCode::Char('e') => {
                let act = self.open_editor_action();
                (true, act)
            }
            KeyCode::Char('f') => {
                self.filter_index = (self.filter_index + 1) % 3;
                self.selected = 0;
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

pub fn render_tasks_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdTasksSectionState,
    palette: &Palette,
) -> ShepherdTasksHits {
    let mut hits = ShepherdTasksHits::default();
    if area.width < 20 || area.height < 5 {
        return hits;
    }

    // Responsive split: if width < 70, compact mode
    let list_width = if area.width >= 100 {
        (area.width * 32 / 100).max(28)
    } else if area.width >= 70 {
        (area.width * 35 / 100).max(24)
    } else {
        24
    };

    // Prepare list items
    let filtered_indices = state.filtered_indices();
    let list_items: Vec<ListViewItem> = filtered_indices
        .iter()
        .map(|&idx| {
            let t = &state.tasks[idx];
            let badge_color = match t.state.to_lowercase().as_str() {
                "open" | "active" => palette.green,
                "done" | "completed" => palette.overlay0,
                "blocked" => palette.red,
                _ => palette.yellow,
            };
            ListViewItem {
                title: format!("#{:02} {}", t.id, t.title),
                badge: Some((
                    format!("[{}]", t.state.to_uppercase()),
                    Style::default().fg(badge_color),
                )),
                meta: None,
            }
        })
        .collect();

    let filter_tabs = ["Todas", "Abiertas", "Completadas"];
    let split = split_panes(
        area,
        list_width,
        compact_panel(
            state.form.is_some() || state.import_gh.is_some() || state.confirm_archive.is_some(),
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
        Some((&filter_tabs, state.filter_index)),
        Some(("+ New", "n")),
        palette,
    );

    // If terminal is wide enough, render right panel (detail or form)
    if let Some(detail_area) = split.detail {
        if let Some(sep_x) = split.divider_x {
            draw_divider(
                buffer,
                area,
                sep_x,
                Style::default().fg(palette.surface_dim),
            );
        }

        // 1. If Create Task Form is open
        if let Some(ref form) = state.form {
            let inline_form = InlineFormState {
                title: "CREATE TASK (inline form)".into(),
                fields: vec![
                    FormField {
                        label: "Task title:".into(),
                        value: form.title.clone(),
                        placeholder: "e.g. Refactor the bus event loop".into(),
                    },
                    FormField {
                        label: "Description / Specification:".into(),
                        value: form.description.clone(),
                        placeholder: "Goals and technical design...".into(),
                    },
                    FormField {
                        label: "Acceptance criteria (separated by ';'):".into(),
                        value: form.criteria.clone(),
                        placeholder: "1. Tests without regressions; 2. CLI verification".into(),
                    },
                    FormField {
                        label: "Attachments (paths separated by space):".into(),
                        value: form.attachments.clone(),
                        placeholder: "src/bus.rs tests/bus.rs".into(),
                    },
                ],
                active_field: form.active_field,
                error: form.error.clone(),
                success: if form.validate().is_ok() {
                    Some("Parameters validated.".into())
                } else {
                    None
                },
            };
            let is_valid = form.validate().is_ok();
            hits.form_hits = Some(render_inline_form(
                buffer,
                detail_area,
                &inline_form,
                "Save Task (Enter)",
                "Cancel (Esc)",
                is_valid,
                palette,
            ));
            return hits;
        }

        // 2. If Import GitHub Form is open
        if let Some(ref import_form) = state.import_gh {
            let inline_form = InlineFormState {
                title: "IMPORT GITHUB ISSUE (inline form)".into(),
                fields: vec![FormField {
                    label: "GitHub issue number (#123) or URL:".into(),
                    value: import_form.issue_ref.clone(),
                    placeholder: "e.g. #42 or https://github.com/herdrdev/herdr/issues/42".into(),
                }],
                active_field: 0,
                error: import_form.error.clone(),
                success: if import_form.validate().is_ok() {
                    Some("Valid reference.".into())
                } else {
                    None
                },
            };
            let is_valid = import_form.validate().is_ok();
            hits.form_hits = Some(render_inline_form(
                buffer,
                detail_area,
                &inline_form,
                "Importar Issue (Enter)",
                "Cancel (Esc)",
                is_valid,
                palette,
            ));
            return hits;
        }

        // 3. Normal detail view or empty state
        if let Some(task) = state.selected_task() {
            let mut badges = vec![];
            let badge_color = match task.state.to_lowercase().as_str() {
                "open" | "active" => palette.green,
                "done" | "completed" => palette.overlay0,
                _ => palette.yellow,
            };
            badges.push((
                format!("[{}]", task.state.to_uppercase()),
                Style::default().fg(badge_color),
            ));

            let meta = format!(
                "Status: {}  |  Branch: {}  |  PR: #{}",
                task.state,
                if task.branch.is_empty() {
                    "ninguna"
                } else {
                    &task.branch
                },
                task.pr
            );

            let header = DetailHeader {
                title: format!("TASK #{}: {}", task.id, task.title),
                badges,
                metadata_line: Some(meta),
                actions: vec![("Edit in Pane".to_string(), Some("e".into()))],
            };

            let mut content = vec![];
            if !task.description.is_empty() {
                content.push(DetailContentBlock::Heading("SPECIFICATION:".into()));
                content.push(DetailContentBlock::Paragraph(task.description.clone()));
            }

            if !task.criteria.is_empty() {
                content.push(DetailContentBlock::Heading("ACCEPTANCE CRITERIA:".into()));
                let checklist = task
                    .criteria
                    .iter()
                    .map(|c| {
                        let checked = c.starts_with("- [x]") || c.starts_with("- [X]");
                        let clean = c
                            .trim_start_matches("- [x]")
                            .trim_start_matches("- [X]")
                            .trim_start_matches("- [ ]")
                            .trim_start_matches("- ")
                            .trim();
                        (checked, clean.to_string())
                    })
                    .collect();
                content.push(DetailContentBlock::Checklist(checklist));
            }

            if !task.attachments.is_empty() {
                content.push(DetailContentBlock::Heading("ARCHIVOS ADJUNTOS:".into()));
                content.push(DetailContentBlock::BulletList(task.attachments.clone()));
            }

            // Reserve space for confirmation stripe if confirming archive
            let main_detail_area = if state.confirm_archive.is_some() {
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
                ("Start Run", "r", true),
                ("Archive Task", "a", false),
                ("Importar GitHub", "i", false),
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

            if let Some(archive_id) = state.confirm_archive {
                let stripe_area = Rect::new(
                    detail_area.x,
                    detail_area.bottom().saturating_sub(4),
                    detail_area.width,
                    4,
                );
                hits.stripe_hits = Some(render_confirm_stripe(
                    buffer,
                    stripe_area,
                    &format!("Archive task #{archive_id}? It will be hidden from the active list."),
                    "Confirm Archive (Enter)",
                    "Cancel (Esc)",
                    true,
                    palette,
                ));
            }
        } else {
            // Empty state
            let actions = [
                ("+ Create First Task", Some("n")),
                ("Import from GitHub", Some("i")),
            ];
            hits.empty_hits = Some(render_empty_state(
                buffer,
                detail_area,
                "NO TASKS REGISTERED",
                "No tasks match the current filter.\nYou can add a task manually or import a GitHub issue.",
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

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_tasks_section_state_navigation_and_filters() {
        let mut state = ShepherdTasksSectionState::default();
        state.tasks = vec![
            ShepherdTaskRow {
                id: 1,
                title: "Task 1".into(),
                state: "open".into(),
                ..Default::default()
            },
            ShepherdTaskRow {
                id: 2,
                title: "Task 2".into(),
                state: "done".into(),
                ..Default::default()
            },
            ShepherdTaskRow {
                id: 3,
                title: "Task 3".into(),
                state: "open".into(),
                ..Default::default()
            },
        ];

        assert_eq!(state.filtered_indices().len(), 3);
        state.filter_index = 1; // Open
        assert_eq!(state.filtered_indices(), vec![0, 2]);

        state.select_next();
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_task().unwrap().id, 3);

        state.select_prev();
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_task().unwrap().id, 1);
    }

    #[test]
    fn test_tasks_section_inline_form_and_archive_stripe() {
        let mut state = ShepherdTasksSectionState::default();
        state.tasks = vec![ShepherdTaskRow {
            id: 42,
            title: "Task to archive".into(),
            state: "open".into(),
            ..Default::default()
        }];

        // Start archive
        state.start_archive_task();
        assert_eq!(state.confirm_archive, Some(42));

        // Esc cancels without archiving
        assert!(state.cancel_sub_action());
        assert_eq!(state.confirm_archive, None);
        assert_eq!(state.last_action, None);

        // Start archive again and confirm
        state.start_archive_task();
        let act = state.confirm_archive_task();
        assert_eq!(act, Some(ShepherdTasksAction::ArchiveTask { task_id: 42 }));
        assert_eq!(state.confirm_archive, None);
    }

    #[test]
    fn test_tasks_section_pure_render_and_empty_state() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 100, 20);
        let mut buffer = Buffer::empty(area);

        let state = ShepherdTasksSectionState::default();
        let hits = render_tasks_section(&mut buffer, area, &state, &palette);

        assert_eq!(hits.list_hits.rows.len(), 0);
        assert!(hits.empty_hits.is_some());
        let empty_hits = hits.empty_hits.unwrap();
        assert_eq!(empty_hits.actions.len(), 2);
    }
}
