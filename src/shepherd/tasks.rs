//! Tasks view for Shepherd.
//!
//! Follows Shepherd's UI principles:
//! - Pure state (`ShepherdTasksState`) separated from runtime/I/O.
//! - Pure render (`render_shepherd_tasks_panel`) taking `&ShepherdTasksState` and `&Palette`.
//! - Mouse-first with keyboard shortcuts.
//! - Destructive actions (archive) require explicit confirmation.
//! - Editing opens `$EDITOR` in a new Shepherd pane (never inside this view).
//! - Task creation validates attachments to avoid broken file paths.
//! - GitHub issue import shows clean errors on failure without leaving partial state.

#![allow(dead_code)]

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::app::state::Palette;
use super::panel::contains_point;

pub const SHEPHERD_TASKS_MODAL_SIZE: (u16, u16) = (92, 26);

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TasksSubModal {
    None,
    Create(CreateTaskFormState),
    ImportGh(ImportGhFormState),
    ConfirmArchive {
        task_id: u32,
        title: String,
        message: String,
    },
}

impl Default for TasksSubModal {
    fn default() -> Self {
        Self::None
    }
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
            return Err("El título de la tarea es obligatorio".into());
        }
        if self.description.trim().is_empty() {
            return Err("La descripción o spec es obligatoria".into());
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
                    return Err(format!("Ruta de archivo adjunto no existe: '{}'", p.display()));
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
        self.active_field = if self.active_field == 0 { 3 } else { self.active_field - 1 };
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
            0 => { self.title.pop(); }
            1 => { self.description.pop(); }
            2 => { self.criteria.pop(); }
            3 => { self.attachments.pop(); }
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
            return Err("El número o URL del issue de GitHub es obligatorio".into());
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
    let template_text = "Design decisions, constraints, interfaces. Agents read this before coding.";
    trimmed == "# Spec\n\nDesign decisions, constraints, interfaces. Agents read this before coding."
        || trimmed == "# Spec\nDesign decisions, constraints, interfaces. Agents read this before coding."
        || trimmed == template_text
        || (trimmed.starts_with("# Spec") && trimmed.contains(template_text) && trimmed.len() <= template_text.len() + 20)
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
        } else if let Some(t) = extracted_title {
            t
        } else {
            String::new()
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
                "¿Archivar la tarea '#{id} {title}'? La tarea pasará a la carpeta '_archive' y dejará de mostrarse en la lista activa."
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
            TasksSubModal::Create(form) => {
                match form.validate() {
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
                }
            }
            TasksSubModal::ImportGh(form) => {
                match form.validate() {
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
                }
            }
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

    pub fn handle_click(
        &mut self,
        col: u16,
        row: u16,
        render: &ShepherdTasksRender,
    ) -> Option<ShepherdTasksAction> {
        if contains_point(render.close_button, col, row) {
            self.close_panel();
            return None;
        }

        match self.sub_modal {
            TasksSubModal::None => {
                if contains_point(render.new_task_button, col, row) {
                    self.open_create_modal();
                    return None;
                }
                if contains_point(render.import_gh_button, col, row) {
                    self.open_import_gh_modal();
                    return None;
                }
                if contains_point(render.archive_button, col, row) {
                    self.open_archive_confirm();
                    return None;
                }
                if contains_point(render.edit_button, col, row) {
                    return self.edit_in_editor();
                }
                for (idx, (rect, _task_id)) in render.task_rows.iter().enumerate() {
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
                } else if let TasksSubModal::Create(ref mut form) = self.sub_modal {
                    for (f_idx, f_rect) in render.form_fields.iter().enumerate() {
                        if contains_point(*f_rect, col, row) {
                            form.active_field = f_idx;
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

/// Hit targets and layout recorded during pure render of tasks view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdTasksRender {
    pub area: Rect,
    pub close_button: Rect,
    pub task_rows: Vec<(Rect, u32)>,
    pub new_task_button: Rect,
    pub import_gh_button: Rect,
    pub archive_button: Rect,
    pub edit_button: Rect,
    pub confirm_button: Rect,
    pub cancel_button: Rect,
    pub form_fields: Vec<Rect>,
}

/// Pure render of the Shepherd tasks view.
pub fn render_shepherd_tasks_panel(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdTasksState,
    palette: &Palette,
) -> Option<ShepherdTasksRender> {
    if area.height < 6 || area.width < 20 {
        return Some(ShepherdTasksRender {
            area,
            ..ShepherdTasksRender::default()
        });
    }

    let inner = area;
    let stack = crate::ui::modal_stack_areas(inner, 2, 2, 0, 1);
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);

    // Header
    let button_bar = Rect::new(stack.header.x + 1, stack.header.y, stack.header.width.saturating_sub(2), 1);

    // Action buttons in header (row 0)
    let mut btn_x = button_bar.x;
    let new_task_btn = Rect::new(btn_x, button_bar.y, 16, 1);
    render_btn(buffer, new_task_btn, "+ Nueva (n)", palette.accent, palette);
    btn_x = btn_x.saturating_add(18);

    let import_gh_btn = Rect::new(btn_x, button_bar.y, 18, 1);
    render_btn(buffer, import_gh_btn, "↓ GitHub issue (i)", palette.blue, palette);
    btn_x = btn_x.saturating_add(20);

    let edit_btn = Rect::new(btn_x, button_bar.y, 16, 1);
    render_btn(buffer, edit_btn, "✎ $EDITOR (e)", palette.teal, palette);
    btn_x = btn_x.saturating_add(18);

    let archive_btn = Rect::new(btn_x, button_bar.y, 16, 1);
    render_btn(buffer, archive_btn, "📦 Archivar (a)", palette.yellow, palette);

    // Subtitle in header (row 1)
    let sub_title = format!("{} tarea(s) · vista previa y edición en $EDITOR", state.tasks.len());
    buffer.set_stringn(
        stack.header.x + 1,
        stack.header.y + 1,
        &sub_title,
        usize::from(stack.header.width.saturating_sub(2)),
        base.fg(palette.overlay0),
    );

    // Split content area: Left (List, 38 cols) | Right (Preview, remaining)
    let body = stack.content;
    let list_width = 38.min(body.width.saturating_sub(10));
    let preview_width = body.width.saturating_sub(list_width + 1);

    let list_area = Rect::new(body.x, body.y, list_width, body.height);
    let divider_x = body.x + list_width;
    let preview_area = Rect::new(divider_x + 1, body.y, preview_width, body.height);

    // Draw vertical divider
    for y in body.y..body.bottom() {
        buffer[(divider_x, y)].set_symbol("│").set_style(base.fg(palette.surface_dim));
    }

    // Render task list on the left
    let mut task_rows = Vec::new();
    if state.tasks.is_empty() {
        buffer.set_stringn(
            list_area.x + 1,
            list_area.y + 1,
            "No hay tareas activas.",
            usize::from(list_area.width.saturating_sub(2)),
            base.fg(palette.overlay0),
        );
    } else {
        for (idx, task) in state.tasks.iter().enumerate().take(usize::from(list_area.height)) {
            let row_y = list_area.y + idx as u16;
            let row_rect = Rect::new(list_area.x, row_y, list_area.width, 1);
            let is_sel = idx == state.selected;
            let row_bg = if is_sel { palette.active_row_bg } else { palette.panel_bg };
            let row_style = base.bg(row_bg);

            for rx in row_rect.x..row_rect.right() {
                buffer[(rx, row_y)].set_symbol(" ").set_style(row_style);
            }

            let status_icon = match task.state.as_str() {
                "open" | "pending" => ("○", palette.blue),
                "coding" | "working" => ("●", palette.yellow),
                "reviewing" => ("●", palette.mauve),
                "pr" | "awaiting-approval" => ("●", palette.teal),
                "done" => ("●", palette.green),
                _ => ("·", palette.overlay0),
            };

            let row_text = format!("{} #{:<2} {}", status_icon.0, task.id, task.title);
            buffer.set_stringn(
                row_rect.x + 1,
                row_y,
                &row_text,
                usize::from(row_rect.width.saturating_sub(2)),
                if is_sel {
                    row_style.fg(palette.text).add_modifier(Modifier::BOLD)
                } else {
                    row_style.fg(palette.text)
                },
            );

            task_rows.push((row_rect, task.id));
        }
    }

    // Render task preview on the right
    render_task_preview(buffer, preview_area, state.selected_task(), state.preview_scroll, palette);

    // Footer
    if let Some(footer_area) = stack.footer {
        let footer_line = Line::from(vec![
            Span::styled(" n ", base.fg(palette.accent).add_modifier(Modifier::BOLD)),
            Span::styled("crear  ·  ", base.fg(palette.overlay0)),
            Span::styled(" i ", base.fg(palette.blue).add_modifier(Modifier::BOLD)),
            Span::styled("importar gh  ·  ", base.fg(palette.overlay0)),
            Span::styled(" e ", base.fg(palette.teal).add_modifier(Modifier::BOLD)),
            Span::styled("editar en $EDITOR  ·  ", base.fg(palette.overlay0)),
            Span::styled(" a ", base.fg(palette.yellow).add_modifier(Modifier::BOLD)),
            Span::styled("archivar  ·  ", base.fg(palette.overlay0)),
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
    let mut form_fields = Vec::new();

    // Render submodals if active
    match &state.sub_modal {
        TasksSubModal::None => {}
        TasksSubModal::Create(form) => {
            let (conf, canc, fields) = render_create_task_modal(buffer, inner, form, palette);
            confirm_button = conf;
            cancel_button = canc;
            form_fields = fields;
        }
        TasksSubModal::ImportGh(form) => {
            let (conf, canc, field) = render_import_gh_modal(buffer, inner, form, palette);
            confirm_button = conf;
            cancel_button = canc;
            if let Some(f) = field {
                form_fields.push(f);
            }
        }
        TasksSubModal::ConfirmArchive { task_id, title, message } => {
            let (conf, canc) = render_archive_modal(
                buffer,
                inner,
                &format!("Confirmación: Archivar #{task_id} {title}"),
                message,
                palette,
            );
            confirm_button = conf;
            cancel_button = canc;
        }
    }

    Some(ShepherdTasksRender {
        area: inner,
        close_button: Rect::default(),
        task_rows,
        new_task_button: new_task_btn,
        import_gh_button: import_gh_btn,
        archive_button: archive_btn,
        edit_button: edit_btn,
        confirm_button,
        cancel_button,
        form_fields,
    })
}

fn render_task_preview(
    buffer: &mut Buffer,
    area: Rect,
    task: Option<&ShepherdTaskRow>,
    scroll: u16,
    palette: &Palette,
) {
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    let Some(task) = task else {
        buffer.set_stringn(
            area.x + 1,
            area.y + 1,
            "Selecciona una tarea para ver su preview.",
            usize::from(area.width.saturating_sub(2)),
            base.fg(palette.overlay0),
        );
        return;
    };

    let mut lines = Vec::new();

    // Title header
    lines.push(Line::from(vec![
        Span::styled(format!("# Tarea #{}: ", task.id), base.fg(palette.accent).add_modifier(Modifier::BOLD)),
        Span::styled(&task.title, base.fg(palette.text).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Estado: ", base.fg(palette.overlay0)),
        Span::styled(&task.state, base.fg(palette.teal)),
        Span::styled("  ·  Directorio: ", base.fg(palette.overlay0)),
        Span::styled(task.directory.display().to_string(), base.fg(palette.subtext0)),
    ]));
    if let Some(ref url) = task.issue_url {
        lines.push(Line::from(vec![
            Span::styled("Issue GitHub: ", base.fg(palette.overlay0)),
            Span::styled(url, base.fg(palette.blue)),
        ]));
    }
    lines.push(Line::from(""));

    // Description / Issue markdown preview
    lines.push(Line::from(vec![
        Span::styled("## Descripción", base.fg(palette.mauve).add_modifier(Modifier::BOLD)),
    ]));
    if task.description.trim().is_empty() {
        lines.push(Line::from(vec![Span::styled("  (Sin descripción)", base.fg(palette.overlay0))]));
    } else {
        for l in task.description.lines() {
            if l.starts_with("# ") {
                lines.push(Line::from(vec![Span::styled(l, base.fg(palette.accent).add_modifier(Modifier::BOLD))]));
            } else if l.starts_with("## ") {
                lines.push(Line::from(vec![Span::styled(l, base.fg(palette.mauve).add_modifier(Modifier::BOLD))]));
            } else if l.starts_with("- ") || l.starts_with("* ") {
                lines.push(Line::from(vec![
                    Span::styled("  • ", base.fg(palette.accent)),
                    Span::styled(&l[2..], base.fg(palette.text)),
                ]));
            } else {
                lines.push(Line::from(vec![Span::styled(l, base.fg(palette.text))]));
            }
        }
    }
    lines.push(Line::from(""));

    // Spec only if present and not template
    if !task.spec.trim().is_empty() {
        lines.push(Line::from(vec![
            Span::styled("## Especificación", base.fg(palette.mauve).add_modifier(Modifier::BOLD)),
        ]));
        for l in task.spec.lines() {
            if l.starts_with("# ") {
                lines.push(Line::from(vec![Span::styled(l, base.fg(palette.accent).add_modifier(Modifier::BOLD))]));
            } else if l.starts_with("## ") {
                lines.push(Line::from(vec![Span::styled(l, base.fg(palette.accent).add_modifier(Modifier::BOLD))]));
            } else if l.starts_with("- ") || l.starts_with("* ") {
                lines.push(Line::from(vec![
                    Span::styled("  • ", base.fg(palette.accent)),
                    Span::styled(&l[2..], base.fg(palette.text)),
                ]));
            } else {
                lines.push(Line::from(vec![Span::styled(l, base.fg(palette.text))]));
            }
        }
        lines.push(Line::from(""));
    }

    // Criteria checklist
    lines.push(Line::from(vec![
        Span::styled("## Criterios de Aceptación", base.fg(palette.teal).add_modifier(Modifier::BOLD)),
    ]));
    if task.criteria.is_empty() {
        lines.push(Line::from(vec![Span::styled("  (Sin criterios definidos)", base.fg(palette.overlay0))]));
    } else {
        for crit in &task.criteria {
            let is_checked = crit.starts_with("- [x]") || crit.starts_with("- [X]");
            let (mark, mark_color) = if is_checked {
                ("- [x] ", palette.green)
            } else {
                ("- [ ] ", palette.yellow)
            };
            let text = crit
                .trim_start_matches("- [x] ")
                .trim_start_matches("- [X] ")
                .trim_start_matches("- [ ] ")
                .trim_start_matches("- [x]")
                .trim_start_matches("- [X]")
                .trim_start_matches("- [ ]")
                .trim_start_matches("- ")
                .trim_start_matches("* ")
                .trim();
            lines.push(Line::from(vec![
                Span::styled(format!("  {mark}"), base.fg(mark_color).add_modifier(Modifier::BOLD)),
                Span::styled(text, base.fg(palette.text)),
            ]));
        }
    }

    let viewport_h = usize::from(area.height);
    for (idx, line) in lines.into_iter().skip(usize::from(scroll)).take(viewport_h).enumerate() {
        let row_y = area.y + idx as u16;
        let line_rect = Rect::new(area.x + 1, row_y, area.width.saturating_sub(2), 1);
        Paragraph::new(line).render(line_rect, buffer);
    }
}

fn render_create_task_modal(
    buffer: &mut Buffer,
    container: Rect,
    form: &CreateTaskFormState,
    palette: &Palette,
) -> (Rect, Rect, Vec<Rect>) {
    let modal_width = 76.min(container.width.saturating_sub(4));
    let modal_height = 18.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_tasks_box(buffer, modal_area, palette.panel_bg, palette.accent) {
        Some(r) => r,
        None => return (Rect::default(), Rect::default(), Vec::new()),
    };

    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_stringn(
        inner.x + 1,
        inner.y,
        "Crear Nueva Tarea (Shepherd)",
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.text).add_modifier(Modifier::BOLD),
    );

    let field_names = [
        "Título (*):",
        "Descripción / Spec (*):",
        "Criterios de aceptación (separados por ; o salto de línea):",
        "Archivos adjuntos (rutas existentes en disco, separadas por espacio):",
    ];
    let field_values = [
        &form.title,
        &form.description,
        &form.criteria,
        &form.attachments,
    ];

    let mut field_rects = Vec::new();
    let mut curr_y = inner.y + 2;

    for idx in 0..4 {
        let is_active = form.active_field == idx;
        buffer.set_stringn(
            inner.x + 1,
            curr_y,
            field_names[idx],
            usize::from(inner.width.saturating_sub(2)),
            if is_active {
                base.fg(palette.accent).add_modifier(Modifier::BOLD)
            } else {
                base.fg(palette.overlay0)
            },
        );
        curr_y += 1;

        let input_rect = Rect::new(inner.x + 1, curr_y, inner.width.saturating_sub(2), 1);
        let input_style = if is_active {
            Style::default().fg(palette.text).bg(palette.active_row_bg)
        } else {
            Style::default().fg(palette.text).bg(palette.surface_dim)
        };
        for ix in input_rect.x..input_rect.right() {
            buffer[(ix, input_rect.y)].set_symbol(" ").set_style(input_style);
        }
        let val = field_values[idx];
        let display = if is_active {
            format!("{val}█")
        } else if val.is_empty() {
            format!("<{}>", field_names[idx].trim_end_matches(':'))
        } else {
            val.clone()
        };
        buffer.set_stringn(
            input_rect.x,
            input_rect.y,
            &display,
            usize::from(input_rect.width),
            if val.is_empty() && !is_active {
                input_style.fg(palette.overlay0)
            } else {
                input_style
            },
        );
        field_rects.push(input_rect);
        curr_y += 1;
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

    render_btn(buffer, confirm_btn, " ↵ Crear tarea ", palette.accent, palette);
    render_btn(buffer, cancel_btn, " esc Cancelar ", palette.surface_dim, palette);

    (confirm_btn, cancel_btn, field_rects)
}

fn render_import_gh_modal(
    buffer: &mut Buffer,
    container: Rect,
    form: &ImportGhFormState,
    palette: &Palette,
) -> (Rect, Rect, Option<Rect>) {
    let modal_width = 72.min(container.width.saturating_sub(4));
    let modal_height = 10.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_tasks_box(buffer, modal_area, palette.panel_bg, palette.blue) {
        Some(r) => r,
        None => return (Rect::default(), Rect::default(), None),
    };

    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_stringn(
        inner.x + 1,
        inner.y,
        "Importar Issue de GitHub (mediante gh)",
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.blue).add_modifier(Modifier::BOLD),
    );

    buffer.set_stringn(
        inner.x + 1,
        inner.y + 2,
        "Número de Issue o URL de GitHub:",
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.overlay0),
    );

    let input_rect = Rect::new(inner.x + 1, inner.y + 3, inner.width.saturating_sub(2), 1);
    let input_style = Style::default().fg(palette.text).bg(palette.active_row_bg);
    for ix in input_rect.x..input_rect.right() {
        buffer[(ix, input_rect.y)].set_symbol(" ").set_style(input_style);
    }
    let display = format!("{}█", form.issue_ref);
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

    render_btn(buffer, confirm_btn, " ↵ Importar ", palette.blue, palette);
    render_btn(buffer, cancel_btn, " esc Cancelar ", palette.surface_dim, palette);

    (confirm_btn, cancel_btn, Some(input_rect))
}

fn render_archive_modal(
    buffer: &mut Buffer,
    container: Rect,
    title: &str,
    message: &str,
    palette: &Palette,
) -> (Rect, Rect) {
    let modal_width = 72.min(container.width.saturating_sub(4));
    let modal_height = 9.min(container.height.saturating_sub(2));
    let x = container.x + (container.width.saturating_sub(modal_width)) / 2;
    let y = container.y + (container.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect::new(x, y, modal_width, modal_height);

    let inner = match draw_tasks_box(buffer, modal_area, palette.panel_bg, palette.yellow) {
        Some(r) => r,
        None => return (Rect::default(), Rect::default()),
    };

    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    buffer.set_stringn(
        inner.x + 1,
        inner.y,
        title,
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.yellow).add_modifier(Modifier::BOLD),
    );

    buffer.set_stringn(
        inner.x + 1,
        inner.y + 2,
        message,
        usize::from(inner.width.saturating_sub(2)),
        base.fg(palette.text),
    );

    let btn_y = inner.bottom().saturating_sub(1);
    let confirm_btn = Rect::new(inner.x + 2, btn_y, 18, 1);
    let cancel_btn = Rect::new(inner.x + 22, btn_y, 16, 1);

    render_btn(buffer, confirm_btn, " ↵ Archivar tarea ", palette.yellow, palette);
    render_btn(buffer, cancel_btn, " esc Cancelar ", palette.surface_dim, palette);

    (confirm_btn, cancel_btn)
}

fn render_btn(buffer: &mut Buffer, rect: Rect, label: &str, bg: ratatui::style::Color, palette: &Palette) {
    if rect.width < 2 || rect.height < 1 {
        return;
    }
    let contrast_fg = match bg {
        ratatui::style::Color::Reset => palette.surface_dim,
        color => color,
    };
    let style = Style::default().fg(contrast_fg).bg(bg).add_modifier(Modifier::BOLD);
    buffer.set_style(rect, style);
    buffer.set_stringn(rect.x, rect.y, label, usize::from(rect.width), style);
}

fn draw_tasks_box(
    buffer: &mut Buffer,
    area: Rect,
    bg: ratatui::style::Color,
    border_color: ratatui::style::Color,
) -> Option<Rect> {
    if area.width < 2 || area.height < 2 {
        return None;
    }
    let background = Style::default().bg(bg).remove_modifier(Modifier::DIM);
    let border = Style::default().fg(border_color).bg(bg).remove_modifier(Modifier::DIM);

    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            buffer[(x, y)].set_symbol(" ").set_style(background);
        }
    }
    for x in area.x..area.right() {
        buffer[(x, area.y)]
            .set_symbol(if x == area.x { "┌" } else if x + 1 == area.right() { "┐" } else { "─" })
            .set_style(border);
        let bottom_y = area.bottom() - 1;
        buffer[(x, bottom_y)]
            .set_symbol(if x == area.x { "└" } else if x + 1 == area.right() { "┘" } else { "─" })
            .set_style(border);
    }
    for y in area.y + 1..area.bottom() - 1 {
        buffer[(area.x, y)].set_symbol("│").set_style(border);
        buffer[(area.right() - 1, y)].set_symbol("│").set_style(border);
    }
    Some(Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2))
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
            assert!(form.error.as_ref().unwrap().contains("título"));
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
            assert!(form.error.as_ref().unwrap().contains("descripción"));
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
            assert!(form.error.as_ref().unwrap().contains("no existe"));
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

        if let TasksSubModal::ConfirmArchive { task_id, ref message, .. } = state.sub_modal {
            assert_eq!(task_id, 42);
            assert!(message.contains("Archivar la tarea '#42"));
        } else {
            panic!("Expected ConfirmArchive modal");
        }

        // Cancel with Esc
        let cancel_act = state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(cancel_act, None);
        assert_eq!(state.sub_modal, TasksSubModal::None);
        assert_eq!(state.last_action, None, "Archive must not execute when canceled");

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
        assert!(matches!(act, Some(ShepherdTasksAction::OpenEditor { task_id: 7, .. })));
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
        assert_eq!(act, Some(ShepherdTasksAction::ImportGhIssue { issue_ref: "123".into() }));
        assert_eq!(state.sub_modal, TasksSubModal::None);
    }

    #[test]
    fn test_pure_render_tasks_panel() {
        let task = ShepherdTaskRow {
            id: 1,
            title: "Render Test".into(),
            state: "open".into(),
            description: "Description body line".into(),
            spec: "Spec body line".into(),
            criteria: vec!["- [ ] First check".into()],
            directory: PathBuf::from("/tmp/tasks/1"),
            ..Default::default()
        };
        let state = ShepherdTasksState::new(vec![task]);
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let rendered = render_shepherd_tasks_panel(&mut buffer, area, &state, &palette)
            .expect("should render tasks panel");

        assert!(!rendered.task_rows.is_empty());

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("tarea(s)"));
        assert!(text.contains("Render Test"));
        assert!(text.contains("Descripción"));
        assert!(text.contains("Especificación"));
        assert!(text.contains("Criterios de Aceptación"));
        assert!(text.contains("- [ ] First check"));
    }

    #[test]
    fn test_from_created_task_with_real_files_renders_preview_correctly() {
        let temp_root = std::env::temp_dir().join(format!(
            "shep_task_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&temp_root);

        let new_task = shepherd_core::NewTask {
            id: 0,
            title: "Tarea Real F3e".into(),
            description: "Esta es la descripcion real de la tarea".into(),
            criteria: vec!["Criterio uno".into(), "Criterio dos".into()],
            issue: None,
            issue_markdown: None,
            links: Vec::new(),
        };

        let created = shepherd_core::create_task(&temp_root, new_task).expect("create_task should succeed");
        let row = ShepherdTaskRow::from_created_task(&created);

        assert_eq!(row.title, "Tarea Real F3e");
        assert_eq!(row.description, "Esta es la descripcion real de la tarea");
        assert!(row.spec.is_empty(), "Spec should be empty when it only has the template");
        assert_eq!(row.criteria, vec!["- [ ] Criterio uno", "- [ ] Criterio dos"]);

        let state = ShepherdTasksState::new(vec![row]);
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        render_shepherd_tasks_panel(&mut buffer, area, &state, &palette)
            .expect("should render tasks panel");

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Tarea Real F3e"));
        assert!(text.contains("Esta es la descripcion real de la tarea"));
        assert!(text.contains("- [ ] Criterio uno"));
        assert!(text.contains("- [ ] Criterio dos"));
        assert!(!text.contains("Design decisions, constraints, interfaces"));
        assert!(!text.contains("## Especificación"));

        // Now modify spec.md with custom content and check acceptance criteria
        let custom_spec = "# Spec Custom\n\nDetalles tecnicos de arquitectura.";
        std::fs::write(created.directory.join("spec.md"), custom_spec).unwrap();
        let updated_acceptance = "# Acceptance criteria\n\n- [x] Criterio uno\n- [ ] Criterio dos\n";
        std::fs::write(created.directory.join("acceptance.md"), updated_acceptance).unwrap();

        let updated_row = ShepherdTaskRow::from_created_task(&created);
        assert_eq!(updated_row.spec, custom_spec);
        assert_eq!(updated_row.criteria, vec!["- [x] Criterio uno", "- [ ] Criterio dos"]);

        let updated_state = ShepherdTasksState::new(vec![updated_row]);
        let mut buffer2 = Buffer::empty(area);
        render_shepherd_tasks_panel(&mut buffer2, area, &updated_state, &palette).unwrap();

        let text2 = buffer2
            .content
            .chunks(usize::from(area.width))
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text2.contains("## Especificación"));
        assert!(text2.contains("Detalles tecnicos de arquitectura"));
        assert!(text2.contains("- [x] Criterio uno"));
        assert!(text2.contains("- [ ] Criterio dos"));

        let _ = std::fs::remove_dir_all(&temp_root);
    }
}
