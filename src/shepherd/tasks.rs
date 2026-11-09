//! Tasks view for Shepherd.
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdTasksState`) separated from runtime/I/O.
//! - Mouse-first with keyboard shortcuts.
//! - Destructive actions (archive) require explicit confirmation.
//! - Editing opens `$EDITOR` in a new Shepherd pane (never inside this view).
//! - Task creation validates attachments to avoid broken file paths.
//! - GitHub issue import shows clean errors on failure without leaving partial state.

#![allow(dead_code)]

use std::path::PathBuf;

/// Actions emitted by the Shepherd tasks view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdTasksAction {
    CreateTask {
        title: String,
        description: String,
        criteria: Vec<String>,
        attachments: Vec<PathBuf>,
    },
    ImportGhIssue {
        issue_ref: String,
    },
    ArchiveTask {
        task_id: u32,
    },
    OpenEditor {
        task_id: u32,
        file_path: PathBuf,
    },
}

/// Sub-modals for the tasks view. At most one active at a time.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum TasksSubModal {
    #[default]
    None,
    Create(CreateTaskFormState),
    ImportGh(ImportGhFormState),
    ConfirmArchive {
        task_id: u32,
        title: String,
        message: String,
    },
}

/// Form state for creating a new task.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateTaskFormState {
    pub title: String,
    pub description: String,
    pub criteria: String,
    pub attachments: String,
    pub active_field: usize, // 0: title, 1: description, 2: criteria, 3: attachments
    pub error: Option<String>,
}

impl CreateTaskFormState {
    pub fn validate(&self) -> Result<(Vec<String>, Vec<PathBuf>), String> {
        if self.title.trim().is_empty() {
            return Err("The task title is required".into());
        }
        if self.description.trim().is_empty() {
            return Err("The description or spec is required".into());
        }

        let criteria: Vec<String> = self
            .criteria
            .lines()
            .flat_map(|line| line.split(';'))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        // Validar que no haya rutas de archivos rotas
        let mut valid_attachments = Vec::new();
        if !self.attachments.trim().is_empty() {
            for raw_path in self.attachments.split_whitespace() {
                let p = PathBuf::from(raw_path.trim().trim_matches(','));
                if !p.exists() {
                    return Err(format!(
                        "Attachment file path does not exist: '{}'",
                        p.display()
                    ));
                }
                valid_attachments.push(p);
            }
        }

        Ok((criteria, valid_attachments))
    }

    pub fn cycle_field_forward(&mut self) {
        self.active_field = (self.active_field + 1) % 4;
    }

    pub fn cycle_field_backward(&mut self) {
        self.active_field = if self.active_field == 0 {
            3
        } else {
            self.active_field - 1
        };
    }

    pub fn insert_char(&mut self, c: char) {
        match self.active_field {
            0 => self.title.push(c),
            1 => self.description.push(c),
            2 => self.criteria.push(c),
            3 => self.attachments.push(c),
            _ => {}
        }
    }

    pub fn delete_char(&mut self) {
        match self.active_field {
            0 => {
                self.title.pop();
            }
            1 => {
                self.description.pop();
            }
            2 => {
                self.criteria.pop();
            }
            3 => {
                self.attachments.pop();
            }
            _ => {}
        }
    }
}

/// Form state for importing a GitHub issue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportGhFormState {
    pub issue_ref: String,
    pub error: Option<String>,
}

impl ImportGhFormState {
    pub fn validate(&self) -> Result<String, String> {
        let r = self.issue_ref.trim();
        if r.is_empty() {
            return Err("The GitHub issue number or URL is required".into());
        }
        Ok(r.to_string())
    }

    pub fn insert_char(&mut self, c: char) {
        self.issue_ref.push(c);
    }

    pub fn delete_char(&mut self) {
        self.issue_ref.pop();
    }
}

/// Pure data model of a task row in the TUI view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdTaskRow {
    pub id: u32,
    pub title: String,
    pub state: String,
    pub branch: String,
    pub pr: u32,
    pub issue_url: Option<String>,
    pub issue_markdown: String,
    pub criteria: Vec<String>,
    pub description: String,
    pub spec: String,
    pub attachments: Vec<String>,
    pub directory: PathBuf,
}

pub fn parse_issue_markdown(content: &str) -> (Option<String>, String) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (None, String::new());
    }
    let mut lines = trimmed.lines();
    if let Some(first) = lines.next() {
        if let Some(title) = first.strip_prefix("# ") {
            let rest: String = lines.collect::<Vec<_>>().join("\n");
            return (Some(title.trim().to_string()), rest.trim().to_string());
        }
    }
    (None, trimmed.to_string())
}

pub fn is_spec_template(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return true;
    }
    let template_text =
        "Design decisions, constraints, interfaces. Agents read this before coding.";
    trimmed
        == "# Spec\n\nDesign decisions, constraints, interfaces. Agents read this before coding."
        || trimmed
            == "# Spec\nDesign decisions, constraints, interfaces. Agents read this before coding."
        || trimmed == template_text
        || (trimmed.starts_with("# Spec")
            && trimmed.contains(template_text)
            && trimmed.len() <= template_text.len() + 20)
}

impl ShepherdTaskRow {
    pub fn from_created_task(task: &shepherd_core::CreatedTask) -> Self {
        let issue_url = task.status.issue.as_ref().map(|i| i.url.clone());
        let spec_file = task.directory.join("spec.md");
        let issue_file = task.directory.join("issue.md");
        let acceptance_file = if task.directory.join("acceptance.md").exists() {
            task.directory.join("acceptance.md")
        } else {
            task.directory.join("criteria.md")
        };

        let issue_markdown = std::fs::read_to_string(&issue_file).unwrap_or_default();
        let (extracted_title, description) = parse_issue_markdown(&issue_markdown);

        let title = if !task.status.title.trim().is_empty() {
            task.status.title.clone()
        } else {
            extracted_title.unwrap_or_default()
        };

        let spec_raw = std::fs::read_to_string(&spec_file).unwrap_or_default();
        let spec = if is_spec_template(&spec_raw) {
            String::new()
        } else {
            spec_raw.trim().to_string()
        };

        let criteria: Vec<String> = std::fs::read_to_string(&acceptance_file)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| {
                !l.is_empty()
                    && (l.starts_with("- [ ]")
                        || l.starts_with("- [x]")
                        || l.starts_with("- [X]")
                        || l.starts_with("- ")
                        || l.starts_with("* "))
                    && !l.contains("(add checkable criteria; the reviewer verifies each one)")
                    && !l.starts_with("# ")
                    && !l.starts_with("## ")
            })
            .map(str::to_string)
            .collect();

        Self {
            id: task.status.id,
            title,
            state: task.status.state.clone(),
            branch: task.status.branch.clone(),
            pr: task.status.pr,
            issue_url,
            issue_markdown,
            criteria,
            description,
            spec,
            attachments: Vec::new(),
            directory: task.directory.clone(),
        }
    }
}

/// Pure presentation state for the tasks view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdTasksState {
    pub open: bool,
    pub selected: usize,
    pub scroll: u16,
    pub preview_scroll: u16,
    pub tasks: Vec<ShepherdTaskRow>,
    pub sub_modal: TasksSubModal,
    pub last_action: Option<ShepherdTasksAction>,
    pub error: Option<String>,
}

impl ShepherdTasksState {
    pub fn new(tasks: Vec<ShepherdTaskRow>) -> Self {
        Self {
            open: false,
            selected: 0,
            scroll: 0,
            preview_scroll: 0,
            tasks,
            sub_modal: TasksSubModal::None,
            last_action: None,
            error: None,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if !self.open {
            self.sub_modal = TasksSubModal::None;
        }
    }

    pub fn open_panel(&mut self) {
        self.open = true;
    }

    pub fn close_panel(&mut self) {
        self.open = false;
        self.sub_modal = TasksSubModal::None;
    }

    pub fn select_next(&mut self) {
        if !self.tasks.is_empty() {
            self.selected = (self.selected + 1).min(self.tasks.len().saturating_sub(1));
            self.preview_scroll = 0;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.preview_scroll = 0;
    }

    pub fn select_index(&mut self, index: usize) {
        if index < self.tasks.len() {
            self.selected = index;
            self.preview_scroll = 0;
        }
    }

    pub fn selected_task(&self) -> Option<&ShepherdTaskRow> {
        self.tasks.get(self.selected)
    }

    pub fn scroll_preview_down(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_add(1);
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(1);
    }

    pub fn open_create_modal(&mut self) {
        if matches!(self.sub_modal, TasksSubModal::None) {
            self.sub_modal = TasksSubModal::Create(CreateTaskFormState::default());
        }
    }

    pub fn open_import_gh_modal(&mut self) {
        if matches!(self.sub_modal, TasksSubModal::None) {
            self.sub_modal = TasksSubModal::ImportGh(ImportGhFormState::default());
        }
    }

    pub fn open_archive_confirm(&mut self) {
        if !matches!(self.sub_modal, TasksSubModal::None) {
            return;
        }
        if let Some(task) = self.selected_task() {
            let id = task.id;
            let title = task.title.clone();
            let message = format!(
                "Archive task '#{id} {title}'? It will move to the '_archive' folder and stop showing in the active list."
            );
            self.sub_modal = TasksSubModal::ConfirmArchive {
                task_id: id,
                title,
                message,
            };
        }
    }

    pub fn edit_in_editor(&mut self) -> Option<ShepherdTasksAction> {
        let task = self.selected_task()?;
        let spec_path = task.directory.join("spec.md");
        let path = if spec_path.exists() {
            spec_path
        } else {
            task.directory.join("issue.md")
        };
        let action = ShepherdTasksAction::OpenEditor {
            task_id: task.id,
            file_path: path,
        };
        self.last_action = Some(action.clone());
        Some(action)
    }

    pub fn cancel_sub_modal(&mut self) {
        self.sub_modal = TasksSubModal::None;
    }

    pub fn confirm_active_action(&mut self) -> Option<ShepherdTasksAction> {
        match std::mem::replace(&mut self.sub_modal, TasksSubModal::None) {
            TasksSubModal::None => None,
            TasksSubModal::Create(form) => match form.validate() {
                Ok((criteria, attachments)) => {
                    let action = ShepherdTasksAction::CreateTask {
                        title: form.title.trim().to_string(),
                        description: form.description.trim().to_string(),
                        criteria,
                        attachments,
                    };
                    self.last_action = Some(action.clone());
                    Some(action)
                }
                Err(err) => {
                    let mut form = form;
                    form.error = Some(err);
                    self.sub_modal = TasksSubModal::Create(form);
                    None
                }
            },
            TasksSubModal::ImportGh(form) => match form.validate() {
                Ok(issue_ref) => {
                    let action = ShepherdTasksAction::ImportGhIssue { issue_ref };
                    self.last_action = Some(action.clone());
                    Some(action)
                }
                Err(err) => {
                    let mut form = form;
                    form.error = Some(err);
                    self.sub_modal = TasksSubModal::ImportGh(form);
                    None
                }
            },
            TasksSubModal::ConfirmArchive { task_id, .. } => {
                let action = ShepherdTasksAction::ArchiveTask { task_id };
                self.last_action = Some(action.clone());
                Some(action)
            }
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<ShepherdTasksAction> {
        use crossterm::event::KeyCode;
        match self.sub_modal {
            TasksSubModal::None => match key.code {
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
                KeyCode::Char('n') | KeyCode::Char('c') => {
                    self.open_create_modal();
                    None
                }
                KeyCode::Char('i') => {
                    self.open_import_gh_modal();
                    None
                }
                KeyCode::Char('a') => {
                    self.open_archive_confirm();
                    None
                }
                KeyCode::Char('e') => self.edit_in_editor(),
                KeyCode::PageDown => {
                    self.scroll_preview_down();
                    None
                }
                KeyCode::PageUp => {
                    self.scroll_preview_up();
                    None
                }
                _ => None,
            },
            TasksSubModal::Create(ref mut form) => match key.code {
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
            TasksSubModal::ImportGh(ref mut form) => match key.code {
                KeyCode::Esc => {
                    self.cancel_sub_modal();
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
            TasksSubModal::ConfirmArchive { .. } => match key.code {
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
    fn test_tasks_navigation_and_selection() {
        let t1 = ShepherdTaskRow {
            id: 1,
            title: "Task One".into(),
            state: "open".into(),
            description: "Spec for task one".into(),
            criteria: vec!["- [ ] Criterion 1".into()],
            directory: PathBuf::from("/tmp/tasks/1"),
            ..Default::default()
        };
        let t2 = ShepherdTaskRow {
            id: 2,
            title: "Task Two".into(),
            state: "done".into(),
            description: "Spec for task two".into(),
            criteria: vec!["- [x] Done criterion".into()],
            directory: PathBuf::from("/tmp/tasks/2"),
            ..Default::default()
        };

        let mut state = ShepherdTasksState::new(vec![t1, t2]);
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_task().unwrap().id, 1);

        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_task().unwrap().id, 2);

        state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_create_task_validation_rejects_missing_fields_and_broken_file_paths() {
        let mut state = ShepherdTasksState::new(Vec::new());
        state.open = true;

        // Press 'n' to open create modal
        state.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
        assert!(matches!(state.sub_modal, TasksSubModal::Create(_)));

        // Submit empty form -> fails with title required
        let act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(act, None);
        if let TasksSubModal::Create(ref form) = state.sub_modal {
            assert!(form.error.as_ref().unwrap().contains("title"));
        } else {
            panic!("Expected create modal");
        }

        // Fill title
        for c in "New Feature".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
        // Tab to description
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        // Try enter with empty description -> fails
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        if let TasksSubModal::Create(ref form) = state.sub_modal {
            assert!(form.error.as_ref().unwrap().contains("description"));
        }

        // Fill description
        for c in "Implement feature spec".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }

        // Tab to criteria
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        for c in "Tests pass; Docs updated".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }

        // Tab to attachments, put a NON-EXISTENT path
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        for c in "/non/existent/path/spec.pdf".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }

        // Submit -> fails with broken attachment path error
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        if let TasksSubModal::Create(ref form) = state.sub_modal {
            assert!(form.error.as_ref().unwrap().contains("does not exist"));
        }

        // Fix attachment to point to a real file (Cargo.toml)
        if let TasksSubModal::Create(ref mut form) = state.sub_modal {
            form.attachments = "Cargo.toml".into();
            form.error = None;
        }

        // Submit -> succeeds!
        let act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(state.sub_modal, TasksSubModal::None);
        assert_eq!(
            act,
            Some(ShepherdTasksAction::CreateTask {
                title: "New Feature".into(),
                description: "Implement feature spec".into(),
                criteria: vec!["Tests pass".into(), "Docs updated".into()],
                attachments: vec![PathBuf::from("Cargo.toml")],
            })
        );
    }

    #[test]
    fn test_archive_task_requires_confirmation_destructive_guard() {
        let task = ShepherdTaskRow {
            id: 42,
            title: "Task to archive".into(),
            state: "open".into(),
            directory: PathBuf::from("/tmp/tasks/42"),
            ..Default::default()
        };
        let mut state = ShepherdTasksState::new(vec![task]);
        state.open = true;

        // Press 'a' to archive
        let act = state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
        // Guard: must NOT execute without confirmation!
        assert_eq!(act, None);
        assert_eq!(state.last_action, None);

        if let TasksSubModal::ConfirmArchive {
            task_id,
            ref message,
            ..
        } = state.sub_modal
        {
            assert_eq!(task_id, 42);
            assert!(message.contains("Archive task '#42"));
        } else {
            panic!("Expected ConfirmArchive modal");
        }

        // Cancel with Esc
        let cancel_act = state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(cancel_act, None);
        assert_eq!(state.sub_modal, TasksSubModal::None);
        assert_eq!(
            state.last_action, None,
            "Archive must not execute when canceled"
        );

        // Press 'a' again and confirm with Enter
        state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
        let conf_act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            conf_act,
            Some(ShepherdTasksAction::ArchiveTask { task_id: 42 })
        );
        assert_eq!(state.sub_modal, TasksSubModal::None);
    }

    #[test]
    fn test_open_editor_dispatches_external_editor_action() {
        let task = ShepherdTaskRow {
            id: 7,
            title: "Task with spec".into(),
            directory: PathBuf::from("Cargo.toml"), // existing file
            ..Default::default()
        };
        let mut state = ShepherdTasksState::new(vec![task]);
        state.open = true;

        // Press 'e'
        let act = state.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()));
        assert!(matches!(
            act,
            Some(ShepherdTasksAction::OpenEditor { task_id: 7, .. })
        ));
    }

    #[test]
    fn test_import_gh_modal_validations_and_cancel() {
        let mut state = ShepherdTasksState::new(Vec::new());
        state.open = true;

        // Press 'i'
        state.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::empty()));
        assert!(matches!(state.sub_modal, TasksSubModal::ImportGh(_)));

        // Submit empty -> error
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        if let TasksSubModal::ImportGh(ref form) = state.sub_modal {
            assert!(form.error.is_some());
        }

        // Type issue number: '123'
        for c in "123".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }

        // Submit with Enter
        let act = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            act,
            Some(ShepherdTasksAction::ImportGhIssue {
                issue_ref: "123".into()
            })
        );
        assert_eq!(state.sub_modal, TasksSubModal::None);
    }
}
