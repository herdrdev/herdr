//! Skills, Git, Events and Models: the tabs about configuration and the world around the agents.
//! Loading happens off the UI thread (git and the bus can be slow); the only writes are the model forms.

use crate::panel::{detail_box, list_box, PanelMsg, Tab};
use crate::theme::{card_style, item_style, ACCENT, DIM, OK, WARN};
use crate::Message;
use iced::widget::{button, checkbox, column, container, row, text, text_editor, text_input};
use iced::{Color, Element, Length, Task};
use shepherd_app_core::config_data::{self, Models, SkillRow, TopicRow};
use shepherd_app_core::installed::{self, InstalledRow};
use shepherd_app_core::shepherd_core::bus::Message as BusMessage;
use shepherd_app_core::shepherd_core::config::{Role, Vendor};
use shepherd_app_core::shepherd_core::git::GitRepoStatus;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum CfgMsg {
    Skills(Result<Vec<SkillRow>, String>),
    Git(Result<GitRepoStatus, String>),
    Topics(Result<Vec<TopicRow>, String>),
    Feed(Result<Vec<BusMessage>, String>),
    Models(Result<Models, String>),
    /// What this computer has: agent CLIs, their vendors and models.
    Installed(Vec<InstalledRow>),
    /// The models that the agents list themselves (these arrive after `Installed`).
    InstalledLive(std::collections::HashMap<String, Vec<String>>),
    SelectInstalled,
    /// Start a vendor for an agent that is installed but has none, filled in from what was found.
    AddVendorFrom(String),
    // The window that assigns skills to a role
    SkillPickOpen,
    SkillPickToggle(String),
    SkillPickFilter(String),
    SkillPickDone,
    SkillPickCancel,
    SelectSkill(String),
    // Skill editor
    SkillNewOpen,
    SkillNewName(String),
    SkillNewRepo(bool),
    SkillNewSubmit,
    SkillNewCancel,
    SkillCustomize(String),
    SkillEditOpen(String),
    SkillEdit(text_editor::Action),
    SkillSave,
    SkillCancel,
    /// The first click on Delete warns, the second one deletes.
    SkillDelete(String),
    SelectWorktree(String),
    OpenAgent(String),
    SelectTopic(String),
    SelectVendor(String),
    SelectRole(String),
    VendorField(VField, String),
    VendorMcp(bool),
    VendorDisabled(bool),
    RoleField(RField, String),
    RoleWorktree(bool),
    RoleSpawn(bool),
    NewVendor,
    NewRole,
    Save,
    Delete,
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VField {
    Name,
    Command,
    Args,
    Models,
    DefaultModel,
    ModelFlag,
    ShepherdAgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RField {
    Name,
    Vendor,
    Model,
    Fallback,
    MustDiffer,
    Skills,
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Editing {
    Vendor(String),
    Role(String),
    /// The list of what is installed on this computer.
    Installed,
}

/// A skill's `SKILL.md` being edited.
pub struct SkillEdit {
    name: String,
    file: PathBuf,
    content: text_editor::Content,
    /// The text as it is on disk.
    saved: String,
    /// What Shepherd's validation found in the last save.
    problems: Vec<String>,
}

impl SkillEdit {
    fn open(name: &str, file: PathBuf) -> Result<Self, String> {
        let saved =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(Self {
            name: name.to_owned(),
            content: text_editor::Content::with_text(&saved),
            saved,
            file,
            problems: Vec::new(),
        })
    }

    fn dirty(&self) -> bool {
        self.content.text() != self.saved
    }
}

/// The skills ticked in the window that assigns them to a role.
pub struct SkillPicker {
    selected: Vec<String>,
    filter: String,
}

/// The name and place of a skill about to be created.
pub struct SkillNew {
    name: String,
    /// In the project (`.shepherd/skills`) instead of for the user.
    repo: bool,
    error: Option<String>,
}

#[derive(Default)]
pub struct ConfigPanel {
    installed: Vec<InstalledRow>,
    /// The window that assigns skills to the role being edited, while it is open.
    skill_picker: Option<SkillPicker>,
    /// The skill open in the editor.
    skill_edit: Option<SkillEdit>,
    /// The new-skill form, while it is open.
    skill_new: Option<SkillNew>,
    /// The skill whose Delete was clicked once.
    armed_skill: Option<String>,
    /// The API socket of the session shown; topics and messages are asked of it.
    api: PathBuf,
    skills: Vec<SkillRow>,
    skill: Option<String>,
    git: Option<GitRepoStatus>,
    worktree: Option<String>,
    topics: Vec<TopicRow>,
    topic: Option<String>,
    feed: Vec<BusMessage>,
    models: Models,
    editing: Option<Editing>,
    vendor: Vendor,
    role: Role,
    /// Boxes of the model forms (a list or a half-typed word is not a valid value yet).
    vfields: Vec<(VField, String)>,
    rfields: Vec<(RField, String)>,
    is_new: bool,
    armed_delete: bool,
    pub notice: String,
    pub loading: bool,
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

impl ConfigPanel {
    pub fn load_skills(root: &Path) -> Task<Message> {
        let root = root.to_path_buf();
        Task::perform(async move { config_data::skills(&root) }, |r| {
            Message::Panel(crate::panel::PanelMsg::Cfg(CfgMsg::Skills(r)))
        })
    }

    pub fn load_git(root: &Path) -> Task<Message> {
        let root = root.to_path_buf();
        Task::perform(async move { config_data::git(&root) }, |r| {
            Message::Panel(crate::panel::PanelMsg::Cfg(CfgMsg::Git(r)))
        })
    }

    pub fn set_api(&mut self, api: &Path) {
        if self.api != api {
            self.api = api.to_path_buf();
        }
    }

    pub fn load_topics(api: &Path) -> Task<Message> {
        let api = api.to_path_buf();
        Task::perform(async move { config_data::topics(&api) }, |r| {
            Message::Panel(crate::panel::PanelMsg::Cfg(CfgMsg::Topics(r)))
        })
    }

    /// Skills, topics and models together, so the tab counters are known before those tabs are opened.
    pub fn load_counts(root: &Path, api: &Path) -> Task<Message> {
        Task::batch([
            Self::load_skills(root),
            Self::load_topics(api),
            Self::load_models(root),
        ])
    }

    /// `(skills, topics, vendors)` loaded so far, for the counters on the tabs.
    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.skills.len(),
            self.topics.len(),
            self.models.vendors.len(),
        )
    }

    /// The global `config.yaml` (shown in the status bar), with the home folder as `~`.
    pub fn config_path(&self) -> Option<String> {
        let path = display(&self.models.target);
        if path.is_empty() {
            return None;
        }
        Some(match std::env::var("HOME") {
            Ok(home) if !home.is_empty() && path.starts_with(&home) => {
                format!("~{}", &path[home.len()..])
            }
            _ => path,
        })
    }

    /// Looks again at the topics and at the open topic's messages (called when the state files change).
    pub fn reload_events(&self) -> Task<Message> {
        let mut tasks = vec![Self::load_topics(&self.api)];
        if let Some(name) = self.topic.clone() {
            let api = self.api.clone();
            tasks.push(Task::perform(
                async move { config_data::topic_messages(&api, &name, 200) },
                |r| cfg(CfgMsg::Feed(r)),
            ));
        }
        Task::batch(tasks)
    }

    pub fn load_models(root: &Path) -> Task<Message> {
        let root = root.to_path_buf();
        Task::perform(async move { config_data::models(&root) }, |r| {
            Message::Panel(crate::panel::PanelMsg::Cfg(CfgMsg::Models(r)))
        })
    }

    fn open_vendor(&mut self, name: &str, new: bool) {
        self.editing = Some(Editing::Vendor(name.to_owned()));
        self.vendor = self.models.vendors.get(name).cloned().unwrap_or_default();
        self.is_new = new;
        self.armed_delete = false;
        let v = &self.vendor;
        self.vfields = vec![
            (VField::Name, name.to_owned()),
            (VField::Command, v.command.clone()),
            (VField::Args, v.args.join(" ")),
            (VField::Models, v.models.join(", ")),
            (VField::DefaultModel, v.default_model.clone()),
            (VField::ModelFlag, v.model_flag.clone()),
            (VField::ShepherdAgent, v.shepherd_agent.clone()),
        ];
    }

    fn open_role(&mut self, name: &str, new: bool) {
        self.editing = Some(Editing::Role(name.to_owned()));
        self.role = self.models.roles.get(name).cloned().unwrap_or_default();
        self.is_new = new;
        self.armed_delete = false;
        let r = &self.role;
        self.rfields = vec![
            (RField::Name, name.to_owned()),
            (RField::Vendor, r.vendor.clone()),
            (RField::Model, r.model.clone()),
            (RField::Fallback, r.fallback.join(", ")),
            (RField::MustDiffer, r.must_differ_from.clone()),
            (RField::Skills, r.skills.join(", ")),
            (RField::Prompt, r.prompt.clone()),
        ];
    }

    pub fn update(&mut self, msg: CfgMsg, root: &Path) -> Task<Message> {
        if !matches!(msg, CfgMsg::Delete) {
            self.armed_delete = false;
        }
        match msg {
            CfgMsg::Skills(Ok(skills)) => {
                self.skills = skills;
                self.loading = false;
                // picture aid: `SHEPHERD_NATIVE_SKILL=new` | `customize:<name>` | `edit:<name>`, applied once
                static PICTURE_AID_DONE: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if let Ok(spec) = std::env::var("SHEPHERD_NATIVE_SKILL") {
                    if !PICTURE_AID_DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let msg = match spec.split_once(':') {
                            Some(("customize", n)) => Some(CfgMsg::SkillCustomize(n.to_owned())),
                            Some(("edit", n)) => Some(CfgMsg::SkillEditOpen(n.to_owned())),
                            None if spec == "new" => Some(CfgMsg::SkillNewOpen),
                            _ => None,
                        };
                        if let Some(msg) = msg {
                            if let Some(name) = spec.split_once(':').map(|(_, n)| n.to_owned()) {
                                self.skill = Some(name);
                            }
                            return self.update(msg, root);
                        }
                    }
                }
            }
            CfgMsg::Git(Ok(git)) => {
                self.git = Some(git);
                self.loading = false;
            }
            CfgMsg::Topics(Ok(topics)) => {
                self.topics = topics;
                self.loading = false;
                // like the other tabs, open the first topic instead of an empty detail
                if self.topic.is_none() {
                    if let Some(first) = self.topics.first().map(|t| t.name.clone()) {
                        return self.update(CfgMsg::SelectTopic(first), root);
                    }
                }
            }
            CfgMsg::Models(Ok(models)) => {
                self.models = models;
                self.loading = false;
                if self.editing.is_none() {
                    if let Some(name) = self.models.vendors.keys().next().cloned() {
                        self.open_vendor(&name, false);
                    }
                }
                // picture aid: `SHEPHERD_NATIVE_MODELS=installed` | `role:<name>` | `picker:<name>`, applied once
                static MODELS_AID_DONE: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if let Ok(spec) = std::env::var("SHEPHERD_NATIVE_MODELS") {
                    if !MODELS_AID_DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        if spec == "installed" {
                            self.editing = Some(Editing::Installed);
                        } else if let Some(role) = spec.strip_prefix("role:") {
                            self.open_role(role, false);
                        } else if let Some(role) = spec.strip_prefix("picker:") {
                            self.open_role(role, false);
                            let _ = self.update(CfgMsg::SkillPickOpen, root);
                        }
                    }
                }
                // what is installed depends on the vendors, and asking the shell and the agents takes a moment
                let vendors = self.models.vendors.clone();
                return Task::perform(async move { installed::detect(&vendors) }, |rows| {
                    cfg(CfgMsg::Installed(rows))
                });
            }
            CfgMsg::Installed(rows) => {
                // the slow part comes next: some agents take seconds to list their models
                let asked = rows.clone();
                self.installed = rows;
                return Task::perform(async move { installed::live_for(&asked) }, |live| {
                    cfg(CfgMsg::InstalledLive(live))
                });
            }
            CfgMsg::InstalledLive(live) => installed::merge_live(&mut self.installed, &live),
            CfgMsg::SelectInstalled => {
                self.editing = Some(Editing::Installed);
                self.armed_delete = false;
            }
            CfgMsg::AddVendorFrom(command) => {
                let Some(row) = self
                    .installed
                    .iter()
                    .find(|r| r.command == command)
                    .cloned()
                else {
                    return Task::none();
                };
                let mut name = command.clone();
                let mut n = 2;
                while self.models.vendors.contains_key(&name) {
                    name = format!("{command}-{n}");
                    n += 1;
                }
                self.open_vendor(&name, true);
                let known = installed::KNOWN_KINDS.contains(&command.as_str());
                self.vendor.command = command.clone();
                self.vendor.models = row.models.clone();
                self.vendor.default_model = row.default_model.clone().unwrap_or_default();
                if known {
                    self.vendor.shepherd_agent = command.clone();
                }
                for (field, value) in [
                    (VField::Command, command),
                    (VField::Models, row.models.join(", ")),
                    (VField::DefaultModel, self.vendor.default_model.clone()),
                    (VField::ShepherdAgent, self.vendor.shepherd_agent.clone()),
                ] {
                    if let Some(slot) = self.vfields.iter_mut().find(|(f, _)| *f == field) {
                        slot.1 = value;
                    }
                }
                self.notice = "Filled in from what is installed: pick a name and Save.".into();
            }
            CfgMsg::SkillPickOpen => {
                let selected = self
                    .rfields
                    .iter()
                    .find(|(f, _)| *f == RField::Skills)
                    .map(|(_, v)| words(v))
                    .unwrap_or_default();
                self.skill_picker = Some(SkillPicker {
                    selected,
                    filter: String::new(),
                });
            }
            CfgMsg::SkillPickToggle(name) => {
                if let Some(picker) = &mut self.skill_picker {
                    match picker.selected.iter().position(|s| *s == name) {
                        Some(at) => {
                            picker.selected.remove(at);
                        }
                        None => picker.selected.push(name),
                    }
                }
            }
            CfgMsg::SkillPickFilter(filter) => {
                if let Some(picker) = &mut self.skill_picker {
                    picker.filter = filter;
                }
            }
            CfgMsg::SkillPickCancel => self.skill_picker = None,
            CfgMsg::SkillPickDone => {
                let Some(picker) = self.skill_picker.take() else {
                    return Task::none();
                };
                return self.update(
                    CfgMsg::RoleField(RField::Skills, picker.selected.join(", ")),
                    root,
                );
            }
            CfgMsg::Skills(Err(e))
            | CfgMsg::Git(Err(e))
            | CfgMsg::Topics(Err(e))
            | CfgMsg::Models(Err(e))
            | CfgMsg::Feed(Err(e)) => {
                self.notice = e;
                self.loading = false;
            }
            CfgMsg::Feed(Ok(feed)) => {
                self.feed = feed;
                self.loading = false;
            }
            CfgMsg::SelectSkill(name) => {
                if self.skill_edit.as_ref().is_some_and(SkillEdit::dirty) {
                    self.notice = "Save or cancel the changes to the open skill first.".into();
                } else {
                    self.skill_edit = None;
                    self.skill_new = None;
                    self.armed_skill = None;
                    self.skill = Some(name);
                }
            }
            CfgMsg::SkillNewOpen => {
                self.skill_edit = None;
                self.skill_new = Some(SkillNew {
                    name: String::new(),
                    repo: false,
                    error: None,
                });
            }
            CfgMsg::SkillNewName(name) => {
                if let Some(form) = &mut self.skill_new {
                    form.name = name;
                    form.error = None;
                }
            }
            CfgMsg::SkillNewRepo(repo) => {
                if let Some(form) = &mut self.skill_new {
                    form.repo = repo;
                }
            }
            CfgMsg::SkillNewCancel => self.skill_new = None,
            CfgMsg::SkillNewSubmit => {
                let Some(form) = &mut self.skill_new else {
                    return Task::none();
                };
                let name = form.name.trim().to_owned();
                match config_data::create_skill(root, &name, form.repo)
                    .and_then(|file| SkillEdit::open(&name, file))
                {
                    Ok(edit) => {
                        self.skill_new = None;
                        self.skill = Some(name.clone());
                        self.skill_edit = Some(edit);
                        self.notice = format!("Created skill {name}");
                        return Self::load_skills(root);
                    }
                    Err(why) => form.error = Some(why),
                }
            }
            CfgMsg::SkillCustomize(name) => {
                match config_data::customize_skill(root, &name)
                    .and_then(|file| SkillEdit::open(&name, file))
                {
                    Ok(edit) => {
                        self.skill_edit = Some(edit);
                        self.notice = format!("{name} copied to your skills: it is yours to edit");
                        return Self::load_skills(root);
                    }
                    Err(why) => self.notice = why,
                }
            }
            CfgMsg::SkillEditOpen(name) => {
                match config_data::editable_skill_file(root, &name)
                    .and_then(|file| SkillEdit::open(&name, file))
                {
                    Ok(edit) => self.skill_edit = Some(edit),
                    Err(why) => self.notice = why,
                }
            }
            CfgMsg::SkillEdit(action) => {
                if let Some(edit) = &mut self.skill_edit {
                    edit.content.perform(action);
                }
            }
            CfgMsg::SkillCancel => self.skill_edit = None,
            CfgMsg::SkillSave => {
                let Some(edit) = &mut self.skill_edit else {
                    return Task::none();
                };
                let text = edit.content.text();
                match config_data::save_skill(&edit.file, &text) {
                    Ok(problems) => {
                        edit.saved = text;
                        self.notice = if problems.is_empty() {
                            format!("Saved {}", edit.name)
                        } else {
                            format!("Saved {}, with {} problem(s)", edit.name, problems.len())
                        };
                        edit.problems = problems;
                        return Self::load_skills(root);
                    }
                    Err(why) => self.notice = why,
                }
            }
            CfgMsg::SkillDelete(name) => {
                if self.armed_skill.as_deref() != Some(name.as_str()) {
                    self.armed_skill = Some(name);
                    return Task::none();
                }
                self.armed_skill = None;
                match config_data::delete_skill(root, &name) {
                    Ok(()) => {
                        self.skill_edit = None;
                        self.skill = None;
                        self.notice = format!("Deleted skill {name}");
                        return Self::load_skills(root);
                    }
                    Err(why) => self.notice = why,
                }
            }
            CfgMsg::SelectWorktree(path) => self.worktree = Some(path),
            CfgMsg::OpenAgent(id) => {
                return Task::batch([
                    Task::done(Message::Panel(PanelMsg::Tab(Tab::Agents))),
                    Task::done(Message::Panel(PanelMsg::SelectAgent(id))),
                ]);
            }
            CfgMsg::SelectTopic(name) => {
                self.topic = Some(name.clone());
                self.loading = true;
                let api = self.api.clone();
                return Task::perform(
                    async move { config_data::topic_messages(&api, &name, 200) },
                    |r| cfg(CfgMsg::Feed(r)),
                );
            }
            CfgMsg::SelectVendor(name) => self.open_vendor(&name, false),
            CfgMsg::SelectRole(name) => self.open_role(&name, false),
            CfgMsg::NewVendor => self.open_vendor("my-vendor", true),
            CfgMsg::NewRole => self.open_role("my-role", true),
            CfgMsg::VendorField(field, value) => {
                if let Some(slot) = self.vfields.iter_mut().find(|(f, _)| *f == field) {
                    slot.1 = value.clone();
                }
                let v = &mut self.vendor;
                match field {
                    VField::Name => {}
                    VField::Command => v.command = value,
                    VField::Args => v.args = words(&value),
                    VField::Models => v.models = words(&value),
                    VField::DefaultModel => v.default_model = value,
                    VField::ModelFlag => v.model_flag = value,
                    VField::ShepherdAgent => v.shepherd_agent = value,
                }
            }
            CfgMsg::VendorMcp(on) => self.vendor.mcp.supported = on,
            CfgMsg::VendorDisabled(on) => self.vendor.disabled = on,
            CfgMsg::RoleField(field, value) => {
                if let Some(slot) = self.rfields.iter_mut().find(|(f, _)| *f == field) {
                    slot.1 = value.clone();
                }
                let r = &mut self.role;
                match field {
                    RField::Name => {}
                    RField::Vendor => r.vendor = value,
                    RField::Model => r.model = value,
                    RField::Fallback => r.fallback = words(&value),
                    RField::MustDiffer => r.must_differ_from = value,
                    RField::Skills => r.skills = words(&value),
                    RField::Prompt => r.prompt = value,
                }
            }
            CfgMsg::RoleWorktree(on) => self.role.worktree = on,
            CfgMsg::RoleSpawn(on) => self.role.can_spawn = on,
            CfgMsg::Save => {
                let result = match &self.editing {
                    Some(Editing::Vendor(_)) => {
                        let name = self
                            .vfields
                            .iter()
                            .find(|(f, _)| *f == VField::Name)
                            .map(|(_, v)| v.trim().to_owned())
                            .unwrap_or_default();
                        config_data::save_vendor(&name, &self.vendor)
                            .map(|p| (p, Editing::Vendor(name)))
                    }
                    Some(Editing::Role(_)) => {
                        let name = self
                            .rfields
                            .iter()
                            .find(|(f, _)| *f == RField::Name)
                            .map(|(_, v)| v.trim().to_owned())
                            .unwrap_or_default();
                        config_data::save_role(&name, &self.role).map(|p| (p, Editing::Role(name)))
                    }
                    Some(Editing::Installed) | None => return Task::none(),
                };
                match result {
                    Ok((path, editing)) => {
                        self.notice = format!("Saved in {}", display(&path));
                        self.editing = Some(editing);
                        self.is_new = false;
                        return Self::load_models(root);
                    }
                    Err(e) => self.notice = e,
                }
            }
            CfgMsg::Delete => {
                if !self.armed_delete {
                    self.armed_delete = true;
                    return Task::none();
                }
                self.armed_delete = false;
                if matches!(self.editing, Some(Editing::Installed) | None) {
                    return Task::none();
                }
                let result = match self.editing.take() {
                    Some(Editing::Vendor(name)) => config_data::delete_entry("vendors", &name),
                    Some(Editing::Role(name)) => config_data::delete_entry("roles", &name),
                    Some(Editing::Installed) | None => return Task::none(),
                };
                match result {
                    Ok(path) => {
                        self.notice = format!("Deleted from {}", display(&path));
                        return Self::load_models(root);
                    }
                    Err(e) => self.notice = e,
                }
            }
            CfgMsg::Refresh => {
                self.loading = true;
                return Task::batch([
                    Self::load_git(root),
                    Self::load_skills(root),
                    Self::load_topics(&self.api),
                    Self::load_models(root),
                ]);
            }
        }
        Task::none()
    }

    // ---- views -------------------------------------------------------------------------------------------------

    pub fn skills_view<'a>(&'a self) -> Element<'a, Message> {
        if self.loading && self.skills.is_empty() {
            return loading_state("Loading skills…");
        }
        let enabled = self.skills.iter().filter(|s| s.enabled).count();
        let problems = self
            .skills
            .iter()
            .filter(|s| !s.problems.is_empty())
            .count();
        let summary = container(
            row![
                config_metric("SKILLS", self.skills.len(), ACCENT),
                config_metric("ENABLED", enabled, OK),
                config_metric("ISSUES", problems, if problems > 0 { WARN } else { OK }),
            ]
            .spacing(1),
        )
        .style(crate::theme::kpi_strip_style);
        let items = self
            .skills
            .iter()
            .map(|s| {
                let body = column![
                    text(format!("{}{}", if s.enabled { "" } else { "✖ " }, s.name)).size(13),
                    text(format!(
                        "{}{}",
                        s.scope,
                        if s.global { " · global" } else { "" }
                    ))
                    .size(11)
                    .color(DIM)
                ];
                button(body)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(item_style(self.skill.as_deref() == Some(s.name.as_str())))
                    .on_press(cfg(CfgMsg::SelectSkill(s.name.clone())))
                    .into()
            })
            .collect();
        let detail: Element<'a, Message> = match self
            .skill
            .as_deref()
            .and_then(|n| self.skills.iter().find(|s| s.name == n))
            .or_else(|| self.skills.first())
        {
            None => empty_state(
                "No skills found",
                "Skills will appear here when Shepherd discovers them in this project.",
            ),
            Some(s) => {
                let mut col = column![
                    row![
                        text(format!("SKILL {}", s.name))
                            .size(18)
                            .color(crate::theme::HI),
                        crate::panel::badge(
                            if s.enabled { "ENABLED" } else { "DISABLED" },
                            if s.enabled { OK } else { crate::theme::BAD }
                        ),
                        text("SKILL.md").size(11).color(DIM),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                    text(s.description.clone()).size(13).color(DIM)
                ]
                .spacing(10);
                for problem in &s.problems {
                    col = col.push(crate::panel::alert(
                        text(format!("✕ {problem}"))
                            .size(13)
                            .color(crate::theme::BAD)
                            .into(),
                        crate::theme::BAD,
                    ));
                }
                // what can be done with it: the ones that ship with Shepherd are customized (copied), not edited
                let editable = s.scope != "Default";
                let armed = self.armed_skill.as_deref() == Some(s.name.as_str());
                let mut actions = row![].spacing(10);
                if editable {
                    actions = actions
                        .push(cfg_btn("Edit", CfgMsg::SkillEditOpen(s.name.clone()), true))
                        .push(if armed {
                            button(
                                container(text("Confirm: delete this skill").size(13))
                                    .center_y(Length::Fill),
                            )
                            .padding([0, 12])
                            .height(34)
                            .style(crate::theme::danger_button_style())
                            .on_press(cfg(CfgMsg::SkillDelete(s.name.clone())))
                        } else {
                            button(container(text("Delete").size(13)).center_y(Length::Fill))
                                .padding([0, 12])
                                .height(34)
                                .style(crate::theme::outline_button_style(false))
                                .on_press(cfg(CfgMsg::SkillDelete(s.name.clone())))
                        });
                } else {
                    actions = actions
                        .push(cfg_btn(
                            "Customize",
                            CfgMsg::SkillCustomize(s.name.clone()),
                            true,
                        ))
                        .push(
                            text("ships with Shepherd: customizing copies it to your skills")
                                .size(11)
                                .color(DIM),
                        );
                }
                col.push(actions).push(markdown(&s.body)).into()
            }
        };
        let footer = vec![
            crate::panel::btn_link("+ Skill", PanelMsg::Cfg(CfgMsg::SkillNewOpen)),
            refresh_button(),
        ];
        let detail_pane: Element<'a, Message> =
            if let Some(form) = &self.skill_new {
                detail_box(self.skill_new_view(form))
            } else if let Some(edit) = self.skill_edit.as_ref().filter(|e| {
                self.skill.as_deref().or(Some(e.name.as_str())) == Some(e.name.as_str())
            }) {
                // the editor scrolls by itself (both ways), so it does not go inside the detail's scrollable
                container(self.skill_editor_view(edit))
                    .padding([20, 24])
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                detail_box(detail)
            };
        column![
            summary,
            row![list_box(items, footer), detail_pane].height(Length::Fill),
        ]
        .into()
    }

    /// The form that creates a skill: a name and where it goes.
    fn skill_new_view<'a>(&'a self, form: &'a SkillNew) -> Element<'a, Message> {
        let mut col = column![
            text("New skill").size(18).color(crate::theme::HI),
            text("It starts from Shepherd's template; you edit it next.")
                .size(12)
                .color(DIM),
            column![
                crate::theme::label("NAME"),
                text_input("lowercase-with-dashes", &form.name)
                    .on_input(|n| cfg(CfgMsg::SkillNewName(n)))
                    .on_submit(cfg(CfgMsg::SkillNewSubmit))
                    .style(crate::theme::input_style)
                    .padding([8, 10])
                    .size(13),
            ]
            .spacing(6),
            column![
                crate::theme::label("WHERE"),
                row![
                    checkbox(!form.repo)
                        .label("for me (all projects)")
                        .on_toggle(|on| cfg(CfgMsg::SkillNewRepo(!on)))
                        .size(14)
                        .text_size(13),
                    checkbox(form.repo)
                        .label("in this project")
                        .on_toggle(|on| cfg(CfgMsg::SkillNewRepo(on)))
                        .size(14)
                        .text_size(13),
                ]
                .spacing(24),
            ]
            .spacing(8),
        ]
        .spacing(14);
        if let Some(error) = &form.error {
            col = col.push(crate::panel::alert(
                text(format!("✕ {error}"))
                    .size(13)
                    .color(crate::theme::BAD)
                    .into(),
                crate::theme::BAD,
            ));
        }
        col.push(
            row![
                cfg_btn("Create", CfgMsg::SkillNewSubmit, true),
                cfg_btn("Cancel", CfgMsg::SkillNewCancel, false),
            ]
            .spacing(10),
        )
        .max_width(640)
        .into()
    }

    /// The editor of a skill: title, what the validation found, Save and Cancel, then the text with line numbers.
    fn skill_editor_view<'a>(&'a self, edit: &'a SkillEdit) -> Element<'a, Message> {
        let dirty = edit.dirty();
        let mut head = row![
            text(format!("EDIT {}", edit.name))
                .size(18)
                .color(crate::theme::HI),
            text("SKILL.md").size(11).color(DIM),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);
        if dirty {
            head = head.push(text("● unsaved changes").size(12).color(WARN));
        }
        head = head
            .push(container(text("")).width(Length::Fill))
            .push(cfg_btn("Save", CfgMsg::SkillSave, true))
            .push(cfg_btn(
                if dirty { "Discard" } else { "Close" },
                CfgMsg::SkillCancel,
                false,
            ));
        let mut col = column![head.spacing(10)].spacing(12);
        for problem in &edit.problems {
            col = col.push(crate::panel::alert(
                text(format!("✕ {problem}"))
                    .size(13)
                    .color(crate::theme::BAD)
                    .into(),
                crate::theme::BAD,
            ));
        }
        col.push(crate::editor::view(&edit.content, |a| {
            cfg(CfgMsg::SkillEdit(a))
        }))
        .height(Length::Fill)
        .into()
    }

    pub fn git_view<'a>(&'a self) -> Element<'a, Message> {
        let Some(git) = &self.git else {
            if self.loading {
                return loading_state("Loading Git worktrees…");
            }
            return row![
                list_box(vec![], vec![refresh_button()]),
                detail_box(text("Loading…").color(DIM).into())
            ]
            .into();
        };
        let dirty = git.worktrees.iter().filter(|w| !w.is_clean).count();
        let ahead = git.worktrees.iter().filter(|w| w.ahead > 0).count();
        let behind = git.worktrees.iter().filter(|w| w.behind > 0).count();
        let summary = container(
            row![
                config_metric("WORKTREES", git.worktrees.len(), ACCENT),
                config_metric(
                    "CLEAN",
                    git.worktrees.iter().filter(|w| w.is_clean).count(),
                    OK,
                ),
                config_metric("DIRTY", dirty, if dirty > 0 { WARN } else { OK }),
                config_metric(
                    "SYNC",
                    ahead + behind,
                    if ahead + behind > 0 { WARN } else { OK }
                ),
            ]
            .spacing(1),
        )
        .style(crate::theme::kpi_strip_style);
        let items = git
            .worktrees
            .iter()
            .map(|w| {
                let state = if w.is_clean {
                    "clean".to_owned()
                } else {
                    format!("dirty {}", w.dirty_count)
                };
                let sync = if w.ahead > 0 || w.behind > 0 {
                    format!(" · +{} -{}", w.ahead, w.behind)
                } else {
                    String::new()
                };
                let body = column![
                    text(if w.branch.is_empty() {
                        "(no branch)".to_owned()
                    } else {
                        w.branch.clone()
                    })
                    .size(13),
                    text(format!(
                        "{state}{sync}{}",
                        if w.is_main { " · main" } else { "" }
                    ))
                    .size(11)
                    .color(DIM)
                ];
                button(body)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(item_style(
                        self.worktree.as_deref() == Some(w.path.as_str()),
                    ))
                    .on_press(cfg(CfgMsg::SelectWorktree(w.path.clone())))
                    .into()
            })
            .collect();
        let mut note = column![].spacing(4);
        if let Some(error) = &git.error {
            note = note.push(crate::panel::alert(
                text(format!("✕ {error}"))
                    .size(12)
                    .color(crate::theme::BAD)
                    .into(),
                crate::theme::BAD,
            ));
        }
        if let Some(n) = git
            .gh_note
            .as_deref()
            .filter(|n| !n.is_empty())
            .or(if git.gh_available {
                None
            } else {
                Some("`gh` is not available: pull requests are hidden.")
            })
        {
            note = note.push(text(n.to_owned()).size(12).color(DIM));
        }
        let detail: Element<'a, Message> = match self
            .worktree
            .as_deref()
            .and_then(|p| git.worktrees.iter().find(|w| w.path == p))
            .or_else(|| git.worktrees.first())
        {
            None => note
                .push(empty_state(
                    "No worktrees found",
                    "This project does not currently expose any Git worktrees.",
                ))
                .into(),
            Some(w) => {
                let sync_label = match (w.ahead, w.behind) {
                    (0, 0) => "in sync".to_owned(),
                    (ahead, behind) => format!("+{ahead} ahead · {behind} behind"),
                };
                let mut col = column![
                    row![
                        text(if w.branch.is_empty() {
                            "(detached HEAD)".to_owned()
                        } else {
                            w.branch.clone()
                        })
                        .size(18)
                        .color(crate::theme::HI),
                        crate::panel::badge(if w.is_main { "MAIN" } else { "LINKED" }, ACCENT),
                        crate::panel::badge(
                            if w.is_clean {
                                "CLEAN".to_owned()
                            } else {
                                format!("DIRTY {}", w.dirty_count)
                            },
                            if w.is_clean { OK } else { WARN }
                        ),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                    text(w.path.clone()).size(12).color(DIM),
                    text(sync_label)
                        .size(12)
                        .color(if w.ahead > 0 || w.behind > 0 {
                            WARN
                        } else {
                            OK
                        }),
                ]
                .spacing(8);
                if let Some(upstream) = &w.upstream {
                    col = col.push(text(format!("Upstream: {upstream}")).size(11).color(DIM));
                }
                let review_state = if !w.is_clean {
                    ("Needs review", WARN)
                } else if w.ahead > 0 {
                    ("Ready to inspect", ACCENT)
                } else {
                    ("No local changes", OK)
                };
                col = col.push(crate::panel::card(
                    "CHANGE SUMMARY",
                    None,
                    column![
                        row![
                            text(review_state.0).size(13).color(review_state.1),
                            text(format!(
                                "{} changed file(s) · {} local commit(s)",
                                w.dirty_count, w.ahead
                            ))
                            .size(11)
                            .color(DIM),
                        ]
                        .spacing(10),
                        text(if w.behind > 0 {
                            format!("{} commit(s) behind upstream", w.behind)
                        } else {
                            "Branch is not behind upstream.".to_owned()
                        })
                        .size(11)
                        .color(if w.behind > 0 { WARN } else { DIM }),
                    ]
                    .spacing(4)
                    .into(),
                ));
                if let Some(agent) = &w.agent_id {
                    col = col.push(crate::panel::card(
                        "AGENT CONTEXT",
                        Some(crate::panel::btn_link(
                            "Open agent workspace",
                            crate::panel::PanelMsg::Cfg(CfgMsg::OpenAgent(agent.clone())),
                        )),
                        column![
                            text(agent.clone()).size(13).color(ACCENT),
                            text("This worktree is linked to a Shepherd agent.")
                                .size(11)
                                .color(DIM),
                        ]
                        .spacing(4)
                        .into(),
                    ));
                }
                if let Some(pr) = &w.pr {
                    col = col.push(crate::panel::card(
                        "PULL REQUEST",
                        None,
                        column![
                            text(format!("#{} {}", pr.number, pr.title)).size(13),
                            text(pr.state.clone())
                                .size(11)
                                .color(if pr.state == "OPEN" { OK } else { DIM }),
                        ]
                        .spacing(4)
                        .into(),
                    ));
                } else {
                    col = col.push(
                        text("No pull request detected for this worktree.")
                            .size(11)
                            .color(DIM),
                    );
                }
                col.push(note).max_width(900).into()
            }
        };
        column![
            summary,
            row![list_box(items, vec![refresh_button()]), detail_box(detail)].height(Length::Fill),
        ]
        .into()
    }

    pub fn events_view<'a>(&'a self, frame: u32) -> Element<'a, Message> {
        if self.loading && self.topics.is_empty() {
            return loading_state("Loading event topics…");
        }
        let paused = self.topics.iter().filter(|t| t.paused).count();
        let unread: u64 = self
            .topics
            .iter()
            .flat_map(|t| t.groups.iter())
            .flat_map(|g| g.lag_by_partition.values())
            .sum();
        let summary = container(
            row![
                config_metric("TOPICS", self.topics.len(), ACCENT),
                config_metric("PAUSED", paused, if paused > 0 { WARN } else { OK }),
                config_metric(
                    "UNREAD",
                    unread as usize,
                    if unread > 0 { WARN } else { OK }
                ),
            ]
            .spacing(1),
        )
        .style(crate::theme::kpi_strip_style);
        let items = self
            .topics
            .iter()
            .map(|t| {
                let unread: u64 = t
                    .groups
                    .iter()
                    .flat_map(|g| g.lag_by_partition.values())
                    .sum();
                let mut title = row![text(format!("#{}", t.name))
                    .size(13)
                    .color(crate::theme::HI)]
                .spacing(8)
                .align_y(iced::Alignment::Center);
                if t.paused {
                    title = title.push(text("paused").size(11).color(WARN));
                }
                if unread > 0 {
                    title = title.push(text(format!("{unread} unread")).size(11).color(WARN));
                }
                let body = column![
                    title,
                    text(format!(
                        "{} messages · {} partition(s)",
                        t.messages_count, t.partitions
                    ))
                    .size(11)
                    .color(DIM)
                ]
                .spacing(1);
                button(body)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(item_style(self.topic.as_deref() == Some(t.name.as_str())))
                    .on_press(cfg(CfgMsg::SelectTopic(t.name.clone())))
                    .into()
            })
            .collect();
        let detail: Element<'a, Message> = match self
            .topic
            .as_deref()
            .and_then(|n| self.topics.iter().find(|t| t.name == n))
        {
            None => empty_state(
                "No topic selected",
                "Select an event topic to inspect consumers, lag, and recent messages.",
            ),
            Some(t) => {
                let consumers: Vec<String> = t
                    .groups
                    .iter()
                    .flat_map(|g| g.members.iter().cloned())
                    .collect();
                let mut col = column![row![
                    text(format!("#{}", t.name))
                        .size(18)
                        .color(crate::theme::HI),
                    text(format!(
                        "{} messages · consumers {}",
                        t.messages_count,
                        if consumers.is_empty() {
                            "none".to_owned()
                        } else {
                            consumers.join(" · ")
                        }
                    ))
                    .size(12)
                    .color(DIM),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center)]
                .spacing(16);
                // Partitions: messages (offset consumed + still unread, best known over the groups) and lag.
                let lag_of = |p: u32| -> u64 {
                    t.groups
                        .iter()
                        .map(|g| g.lag_by_partition.get(&p).copied().unwrap_or(0))
                        .sum()
                };
                let msgs_of = |p: u32| -> u64 {
                    let groups = t
                        .groups
                        .iter()
                        .map(|g| {
                            g.committed_offsets.get(&p).copied().unwrap_or(0)
                                + g.lag_by_partition.get(&p).copied().unwrap_or(0)
                        })
                        .max()
                        .unwrap_or(0);
                    // without consumers, the newest offset seen in the recent messages
                    let seen = self
                        .feed
                        .iter()
                        .filter(|m| m.partition == p)
                        .map(|m| m.offset + 1)
                        .max()
                        .unwrap_or(0);
                    groups.max(seen)
                };
                let max_msgs = (0..t.partitions).map(msgs_of).max().unwrap_or(0).max(1);
                let mut parts = column![].spacing(8);
                for p in 0..t.partitions {
                    let (lag, msgs) = (lag_of(p), msgs_of(p));
                    // `lag` canvas: the stream is as long as the partition is full
                    let reach = 0.15 + 0.8 * msgs as f64 / max_msgs as f64;
                    parts = parts.push(
                        row![
                            text(format!("p{p}"))
                                .size(12)
                                .color(crate::theme::HI)
                                .width(60),
                            iced::widget::canvas::Canvas::new(crate::prism::Prism {
                                preset: crate::prism::Preset::lag(reach, f64::from(p) * 7.0),
                                frame: frame.wrapping_add(p * 3),
                            })
                            .width(Length::Fill)
                            .height(14),
                            row![
                                text(format!("{msgs} msgs · lag ")).size(12).color(DIM),
                                text(lag.to_string()).size(12).color(if lag > 1 {
                                    WARN
                                } else {
                                    OK
                                }),
                            ]
                            .width(150),
                        ]
                        .spacing(12)
                        .align_y(iced::Alignment::Center),
                    );
                }
                col = col.push(crate::panel::card(
                    "PARTITIONS · LAG",
                    Some(text("messages / lag").size(11).color(DIM).into()),
                    parts.into(),
                ));
                // Recent messages, newest first: time, who emitted it, and colours.
                let mut feed = column![].spacing(0);
                if self.feed.is_empty() {
                    feed = feed.push(text("No messages yet.").size(12).color(DIM));
                }
                for m in self.feed.iter().rev() {
                    feed = feed.push(crate::panel::message_row(m, true));
                }
                col = col.push(crate::panel::card(
                    "RECENT MESSAGES",
                    Some(
                        text(format!("{} shown", self.feed.len()))
                            .size(11)
                            .color(DIM)
                            .into(),
                    ),
                    feed.into(),
                ));
                col.max_width(1000).into()
            }
        };
        column![
            summary,
            row![list_box(items, vec![refresh_button()]), detail_box(detail)].height(Length::Fill),
        ]
        .into()
    }

    fn vbox<'a>(&'a self, label: &'static str, field: VField) -> Element<'a, Message> {
        let value = self
            .vfields
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        row![
            iced::widget::container(text(label).size(12).color(DIM)).width(110),
            text_input("", value)
                .on_input(move |v| cfg(CfgMsg::VendorField(field, v)))
                .size(13)
                .padding([8, 10])
                .style(crate::theme::input_style)
                .width(Length::Fill)
        ]
        .spacing(8)
        .into()
    }

    fn rbox<'a>(&'a self, label: &'static str, field: RField) -> Element<'a, Message> {
        let value = self
            .rfields
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        row![
            iced::widget::container(text(label).size(12).color(DIM)).width(110),
            text_input("", value)
                .on_input(move |v| cfg(CfgMsg::RoleField(field, v)))
                .size(13)
                .padding([8, 10])
                .style(crate::theme::input_style)
                .width(Length::Fill)
        ]
        .spacing(8)
        .into()
    }

    pub fn models_view<'a>(&'a self) -> Element<'a, Message> {
        if self.loading && self.models.vendors.is_empty() && self.models.roles.is_empty() {
            return loading_state("Loading models and roles…");
        }
        let summary = container(
            row![
                config_metric("VENDORS", self.models.vendors.len(), ACCENT),
                config_metric("ROLES", self.models.roles.len(), OK),
                config_metric(
                    "DISABLED",
                    self.models.vendors.values().filter(|v| v.disabled).count(),
                    WARN,
                ),
            ]
            .spacing(1),
        )
        .style(crate::theme::kpi_strip_style);
        let mut items: Vec<Element<'a, Message>> = vec![
            crate::theme::label("INSTALLED").into(),
            button(column![
                text("On this computer").size(13),
                text(if self.installed.is_empty() {
                    "looking…".to_owned()
                } else {
                    format!(
                        "{} agent(s) installed",
                        self.installed.iter().filter(|r| r.installed()).count()
                    )
                })
                .size(11)
                .color(DIM)
            ])
            .width(Length::Fill)
            .padding([4, 8])
            .style(item_style(matches!(self.editing, Some(Editing::Installed))))
            .on_press(cfg(CfgMsg::SelectInstalled))
            .into(),
            crate::theme::label("VENDORS").into(),
        ];
        for (name, v) in &self.models.vendors {
            let selected = matches!(&self.editing, Some(Editing::Vendor(n)) if n == name);
            items.push(
                button(column![
                    text(name.clone()).size(13),
                    text(v.command.clone()).size(11).color(DIM)
                ])
                .width(Length::Fill)
                .padding([4, 8])
                .style(item_style(selected))
                .on_press(cfg(CfgMsg::SelectVendor(name.clone())))
                .into(),
            );
        }
        items.push(crate::theme::label("ROLES").into());
        for (name, r) in &self.models.roles {
            let selected = matches!(&self.editing, Some(Editing::Role(n)) if n == name);
            items.push(
                button(column![
                    text(name.clone()).size(13),
                    text(format!("{} {}", r.vendor, r.model))
                        .size(11)
                        .color(DIM)
                ])
                .width(Length::Fill)
                .padding([4, 8])
                .style(item_style(selected))
                .on_press(cfg(CfgMsg::SelectRole(name.clone())))
                .into(),
            );
        }
        let footer = vec![
            button(text("+ Vendor").size(12))
                .padding([4, 10])
                .style(item_style(false))
                .on_press(cfg(CfgMsg::NewVendor))
                .into(),
            button(text("+ Role").size(12))
                .padding([4, 10])
                .style(item_style(false))
                .on_press(cfg(CfgMsg::NewRole))
                .into(),
        ];
        let target = format!("Saved in {}", display(&self.models.target));
        let detail: Element<'a, Message> = match &self.editing {
            None => text("Pick a vendor or a role.").color(DIM).into(),
            Some(Editing::Installed) => self.installed_view(),
            Some(editing) => {
                let mut form = column![].spacing(6);
                match editing {
                    Editing::Vendor(_) => {
                        form = form.push(text("VENDOR").size(11).color(DIM));
                        for (label, f) in [
                            ("name", VField::Name),
                            ("command", VField::Command),
                            ("args", VField::Args),
                            ("models", VField::Models),
                            ("default model", VField::DefaultModel),
                            ("model flag", VField::ModelFlag),
                            ("shepherd agent", VField::ShepherdAgent),
                        ] {
                            form = form.push(self.vbox(label, f));
                        }
                        form = form.push(self.model_chips());
                        form = form.push(
                            checkbox(self.vendor.mcp.supported)
                                .label("MCP supported")
                                .on_toggle(|on| cfg(CfgMsg::VendorMcp(on)))
                                .size(14)
                                .text_size(12),
                        );
                        form = form.push(
                            checkbox(self.vendor.disabled)
                                .label("disabled")
                                .on_toggle(|on| cfg(CfgMsg::VendorDisabled(on)))
                                .size(14)
                                .text_size(12),
                        );
                    }
                    Editing::Installed => {}
                    Editing::Role(_) => {
                        form = form.push(text("ROLE").size(11).color(DIM));
                        let vendor_names: Vec<String> =
                            self.models.vendors.keys().cloned().collect();
                        form = form
                            .push(self.rbox("name", RField::Name))
                            .push(self.picked_rbox("vendor", RField::Vendor, vendor_names))
                            .push(self.picked_rbox("model", RField::Model, self.role_models()))
                            .push(self.rbox("fallback", RField::Fallback))
                            .push(self.rbox("must differ from", RField::MustDiffer))
                            .push(self.skills_row())
                            .push(self.rbox("prompt", RField::Prompt));
                        form = form.push(
                            checkbox(self.role.worktree)
                                .label("own git worktree")
                                .on_toggle(|on| cfg(CfgMsg::RoleWorktree(on)))
                                .size(14)
                                .text_size(12),
                        );
                        form = form.push(
                            checkbox(self.role.can_spawn)
                                .label("can start other agents")
                                .on_toggle(|on| cfg(CfgMsg::RoleSpawn(on)))
                                .size(14)
                                .text_size(12),
                        );
                    }
                }
                let delete_label = if self.armed_delete {
                    "Confirm: delete"
                } else {
                    "Delete"
                };
                form.push(row![button(container(text("Save").size(13)).center_y(Length::Fill)).padding([0, 12]).height(34).style(crate::theme::primary_button_style()).on_press(cfg(CfgMsg::Save)), button(container(text(delete_label).size(13)).center_y(Length::Fill)).padding([0, 12]).height(34).style(crate::theme::outline_button_style(!self.armed_delete)).on_press(cfg(CfgMsg::Delete))].spacing(8))
                    .push(text(target).size(11).color(DIM))
                    .push(text(if self.is_new { "New entry: pick a name and Save." } else { "Lists are comma or space separated. Saving rewrites config.yaml (comments are lost, a .bak is kept)." }).size(11).color(DIM))
                    .into()
            }
        };
        let base: Element<'a, Message> = column![
            summary,
            row![list_box(items, footer), detail_box(detail)].height(Length::Fill),
        ]
        .spacing(8)
        .into();
        match &self.skill_picker {
            Some(picker) => iced::widget::stack![base, self.skill_picker_view(picker)].into(),
            None => base,
        }
    }

    /// What is installed on this computer: each agent CLI with its vendor and its models.
    fn installed_view<'a>(&'a self) -> Element<'a, Message> {
        let mut rows = column![].spacing(0);
        if self.installed.is_empty() {
            rows = rows.push(text("Looking at this computer…").size(13).color(DIM));
        }
        for r in &self.installed {
            let (dot, state_text) = match (r.installed(), r.state.as_deref()) {
                (true, Some("available") | None) => (OK, "installed"),
                (true, Some(other)) => (WARN, other),
                (false, _) => (crate::theme::BAD, "not installed"),
            };
            let vendor: Element<'a, Message> = match &r.vendor {
                Some(name) => crate::panel::btn_link(
                    format!("vendor {name}"),
                    PanelMsg::Cfg(CfgMsg::SelectVendor(name.clone())),
                ),
                None => cfg_btn(
                    "Add as vendor",
                    CfgMsg::AddVendorFrom(r.command.clone()),
                    false,
                ),
            };
            let models = if r.models.is_empty() {
                "—".to_owned()
            } else {
                r.models.join(", ")
            };
            rows = rows.push(column![
                container(
                    row![
                        text("●").size(11).color(dot),
                        column![
                            text(r.command.clone()).size(13).color(crate::theme::HI),
                            text(r.path.clone().unwrap_or_else(|| state_text.to_owned()))
                                .size(11)
                                .color(DIM),
                        ]
                        .width(Length::FillPortion(3))
                        .spacing(1),
                        column![
                            text("default model").size(11).color(DIM),
                            text(r.default_model.clone().unwrap_or_else(|| "—".into())).size(13),
                        ]
                        .width(Length::FillPortion(2))
                        .spacing(1),
                        column![text("models").size(11).color(DIM), text(models).size(13)]
                            .width(Length::FillPortion(5))
                            .spacing(1),
                        vendor,
                    ]
                    .spacing(14)
                    .align_y(iced::Alignment::Center),
                )
                .padding([10, 0]),
                container(text(""))
                    .width(Length::Fill)
                    .height(1)
                    .style(|_| container::Style {
                        background: Some(iced::Background::Color(crate::theme::LINE)),
                        ..container::Style::default()
                    }),
            ]);
        }
        column![
            text("Installed on this computer")
                .size(18)
                .color(crate::theme::HI),
            text("Agent CLIs found by your login shell, the vendors Shepherd has for them and the models each offers.")
                .size(12)
                .color(DIM),
            crate::panel::card(
                "AGENTS · VENDORS · MODELS",
                Some(
                    text(format!(
                        "{} installed",
                        self.installed.iter().filter(|r| r.installed()).count()
                    ))
                    .size(11)
                    .color(DIM)
                    .into()
                ),
                rows.into(),
            ),
        ]
        .spacing(14)
        .max_width(1000)
        .into()
    }

    /// Under a vendor's `models`: the models its agent offers that are not in the list yet, one click to add.
    fn model_chips<'a>(&'a self) -> Element<'a, Message> {
        let found = self
            .installed
            .iter()
            .find(|r| !self.vendor.command.is_empty() && r.command == self.vendor.command);
        let mut chips = row![].spacing(6).align_y(iced::Alignment::Center);
        let mut any = false;
        if let Some(found) = found {
            for model in found
                .models
                .iter()
                .filter(|m| !self.vendor.models.contains(m))
            {
                any = true;
                let mut all = self.vendor.models.clone();
                all.push(model.clone());
                chips = chips.push(chip(
                    format!("+ {model}"),
                    CfgMsg::VendorField(VField::Models, all.join(", ")),
                ));
            }
            if let Some(default) = &found.default_model {
                if &self.vendor.default_model != default {
                    any = true;
                    chips = chips.push(chip(
                        format!("default → {default}"),
                        CfgMsg::VendorField(VField::DefaultModel, default.clone()),
                    ));
                }
            }
        }
        if !any {
            return text("").into();
        }
        row![
            container(text("installed").size(12).color(DIM)).width(110),
            chips.wrap()
        ]
        .spacing(8)
        .into()
    }

    /// The models a role can pick: its vendor's list plus what that vendor's agent offers on this computer.
    fn role_models(&self) -> Vec<String> {
        let vendor = self.models.vendors.get(&self.role.vendor);
        let mut models: Vec<String> = vendor.map(|v| v.models.clone()).unwrap_or_default();
        if let Some(v) = vendor {
            if let Some(found) = self.installed.iter().find(|r| r.command == v.command) {
                for m in &found.models {
                    if !models.contains(m) {
                        models.push(m.clone());
                    }
                }
            }
        }
        models
    }

    /// A text field of a role that also offers a list to pick from (the text stays editable).
    fn picked_rbox<'a>(
        &'a self,
        label: &'static str,
        field: RField,
        options: Vec<String>,
    ) -> Element<'a, Message> {
        row![
            self.rbox(label, field),
            iced::widget::pick_list(options, None::<String>, move |choice| {
                cfg(CfgMsg::RoleField(field, choice))
            })
            .placeholder("pick…")
            .width(150),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into()
    }

    /// The skills of a role: what is assigned, and the button that opens the window to change it.
    fn skills_row<'a>(&'a self) -> Element<'a, Message> {
        let assigned = self
            .rfields
            .iter()
            .find(|(f, _)| *f == RField::Skills)
            .map(|(_, v)| words(v))
            .unwrap_or_default();
        row![
            container(text("skills").size(12).color(DIM)).width(110),
            column![
                text(if assigned.is_empty() {
                    "none assigned".to_owned()
                } else {
                    assigned.join(", ")
                })
                .size(13),
                text(format!("{} assigned", assigned.len()))
                    .size(11)
                    .color(DIM),
            ]
            .width(Length::Fill)
            .spacing(1),
            cfg_btn("Assign skills…", CfgMsg::SkillPickOpen, false),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into()
    }

    /// The window that assigns skills to a role: every skill Shepherd has, ticked or not, with a filter.
    fn skill_picker_view<'a>(&'a self, picker: &'a SkillPicker) -> Element<'a, Message> {
        let role = match &self.editing {
            Some(Editing::Role(name)) => name.clone(),
            _ => String::new(),
        };
        let filter = picker.filter.to_lowercase();
        let mut list = column![].spacing(2);
        let mut shown = 0;
        for s in self.skills.iter().filter(|s| {
            filter.is_empty()
                || s.name.to_lowercase().contains(&filter)
                || s.description.to_lowercase().contains(&filter)
        }) {
            shown += 1;
            let ticked = picker.selected.contains(&s.name);
            list = list.push(
                button(
                    row![
                        checkbox(ticked).size(15),
                        column![
                            row![
                                text(s.name.clone()).size(13).color(crate::theme::HI),
                                text(s.scope.to_lowercase()).size(11).color(DIM),
                            ]
                            .spacing(8),
                            text(s.description.chars().take(110).collect::<String>())
                                .size(11)
                                .color(DIM),
                        ]
                        .spacing(1)
                        .width(Length::Fill),
                    ]
                    .spacing(12)
                    .align_y(iced::Alignment::Center),
                )
                .width(Length::Fill)
                .padding([6, 8])
                .style(item_style(ticked))
                .on_press(cfg(CfgMsg::SkillPickToggle(s.name.clone()))),
            );
        }
        // assigned to the role but unknown to the catalog: shown so that they can be taken off
        for missing in picker
            .selected
            .iter()
            .filter(|n| !self.skills.iter().any(|s| &s.name == *n))
        {
            list = list.push(
                button(
                    row![
                        checkbox(true).size(15),
                        text(missing.clone()).size(13),
                        text("not found").size(11).color(WARN),
                    ]
                    .spacing(12)
                    .align_y(iced::Alignment::Center),
                )
                .width(Length::Fill)
                .padding([6, 8])
                .style(item_style(true))
                .on_press(cfg(CfgMsg::SkillPickToggle(missing.clone()))),
            );
        }
        if shown == 0 {
            list = list.push(text("No skill matches.").size(12).color(DIM));
        }
        let dialog = container(
            column![
                row![
                    text(format!("Skills of {role}"))
                        .size(18)
                        .color(crate::theme::HI),
                    container(text("")).width(Length::Fill),
                    text(format!("{} selected", picker.selected.len()))
                        .size(12)
                        .color(ACCENT),
                ]
                .align_y(iced::Alignment::Center),
                text_input("Filter skills", &picker.filter)
                    .on_input(|f| cfg(CfgMsg::SkillPickFilter(f)))
                    .style(crate::theme::input_style)
                    .padding([8, 10])
                    .size(13),
                iced::widget::scrollable(list).height(Length::Fixed(340.0)),
                row![
                    cfg_btn("Done", CfgMsg::SkillPickDone, true),
                    cfg_btn("Cancel", CfgMsg::SkillPickCancel, false),
                    text("Done fills the role's skills; Save keeps them in config.yaml")
                        .size(11)
                        .color(DIM),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(12),
        )
        .padding(18)
        .width(560)
        .style(card_style);
        // `opaque` keeps clicks from reaching what is under the window
        iced::widget::opaque(
            container(dialog)
                .center(Length::Fill)
                .style(|_| container::Style {
                    background: Some(iced::Background::Color(Color::from_rgba8(5, 5, 8, 0.7))),
                    ..container::Style::default()
                }),
        )
    }
}

fn cfg(msg: CfgMsg) -> Message {
    Message::Panel(crate::panel::PanelMsg::Cfg(msg))
}

/// A `.btn` (the primary, bright one or the outlined one) that sends a config message.
fn cfg_btn<'a>(label: &'static str, msg: CfgMsg, primary: bool) -> Element<'a, Message> {
    if primary {
        crate::panel::btn_primary(label, PanelMsg::Cfg(msg))
    } else {
        crate::panel::btn(label, PanelMsg::Cfg(msg))
    }
}

/// A small button that sends a config message (for picking a suggestion).
fn chip<'a>(label: String, msg: CfgMsg) -> Element<'a, Message> {
    button(text(label).size(12))
        .padding([4, 10])
        .style(crate::theme::outline_button_style(false))
        .on_press(cfg(msg))
        .into()
}

fn refresh_button<'a>() -> Element<'a, Message> {
    button(container(text("Refresh").size(13)).center_y(Length::Fill))
        .padding([0, 12])
        .height(34)
        .style(crate::theme::outline_button_style(true))
        .on_press(cfg(CfgMsg::Refresh))
        .into()
}

/// `.empty`: a boxed message with what to do next.
fn empty_state<'a>(title: &'static str, detail: &'static str) -> Element<'a, Message> {
    container(
        column![
            text(title).size(16).color(crate::theme::HI),
            text(detail).size(13).color(DIM),
            refresh_button(),
        ]
        .spacing(8),
    )
    .padding(28)
    .max_width(640)
    .style(card_style)
    .into()
}

/// A small markdown reader for SKILL.md (`.md`): headings, fenced code, list lines and paragraphs.
fn markdown<'a>(body: &str) -> Element<'a, Message> {
    let mut col = column![].spacing(4);
    let mut code: Option<Vec<String>> = None;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(lines) => {
                    col = col.push(
                        container(text(lines.join("\n")).size(12).color(crate::theme::FG))
                            .padding(12)
                            .width(Length::Fill)
                            .style(crate::theme::graph_style),
                    );
                }
                None => code = Some(Vec::new()),
            }
        } else if let Some(lines) = code.as_mut() {
            lines.push(line.to_owned());
        } else if line.starts_with('#') {
            col = col.push(
                container(
                    text(line.to_owned())
                        .font(crate::theme::MONO_BOLD)
                        .size(13)
                        .color(crate::theme::HI),
                )
                .padding(crate::theme::pad(12.0, 0.0, 2.0, 0.0)),
            );
        } else if line.trim().is_empty() {
            col = col.push(container(text("")).height(4));
        } else {
            col = col.push(text(line.to_owned()).size(13).color(crate::theme::FG));
        }
    }
    col.into()
}

fn loading_state<'a>(label: &'static str) -> Element<'a, Message> {
    container(
        column![
            text("●").size(18).color(crate::theme::ACCENT),
            text(label).size(14),
            text("The data is being loaded in the background.")
                .size(12)
                .color(DIM),
        ]
        .spacing(8),
    )
    .padding(20)
    .style(crate::theme::card_style)
    .into()
}

fn config_metric<'a>(label: &'static str, value: usize, color: Color) -> Element<'a, Message> {
    crate::panel::kpi_cell(label, value.to_string(), color, None)
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_split_on_commas_and_spaces_and_ignore_the_empty_bits() {
        assert_eq!(words("a, b  c,,d "), ["a", "b", "c", "d"]);
        assert!(words("  , ").is_empty());
    }
}
