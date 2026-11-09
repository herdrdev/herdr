//! The Shepherd panel: runs (with the pattern launcher and controls), agents, tasks and the monitor of what
//! agents say to each other. It reads `Data` (files) and asks the server for every change.

use crate::pattern_editor::{EditMsg, PatternEditor};
use crate::prism::{Preset, Prism};
use crate::theme::{self, kpi_cell_style, kpi_strip_style};
use crate::theme::{
    card_style, danger_button_style, item_style, pad, primary_button_style, status_color,
    status_glyph, ACCENT, BAD, DIM, FG, LINE, OK, WARN,
};
use crate::{Action, Message};
use iced::widget::canvas::Canvas;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Background, Color, Element, Length, Task};
use shepherd_app_core::actions::{self, RunControl};
use shepherd_app_core::bus_message::line as feed_line;
use shepherd_app_core::data::{bus_feed, ensure_task_prefix, task_of_run, task_prompt, Data};
use shepherd_app_core::pattern_graph::{self, PatternView};
use shepherd_app_core::shepherd_core::bus::Message as BusMessage;
use shepherd_app_core::shepherd_core::service::patterns_run::PatternRunState;
use shepherd_app_core::snapshot::{Snapshot, Status};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Runs,
    Agents,
    Tasks,
    Patterns,
    Monitor,
    Skills,
    Git,
    Events,
    Models,
}

/// A button that needs a second click (no pop-ups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Armed {
    StopRun(String),
    DeleteRun(String),
    CleanRuns,
    CleanBus,
    CleanBusForce,
    ArchiveDone,
    DeleteTask(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone)]
pub enum PanelMsg {
    Tab(Tab),
    SelectRun(String),
    SelectAgent(String),
    SelectTask(u32),
    SelectPattern(String),
    PickAndLaunch(String),
    PatternLoaded(Result<PatternView, String>),
    EditPattern(EditMsg),
    OpenLauncher(Option<u32>),
    CloseLauncher,
    PickPattern(Choice),
    Edit(text_editor::Action),
    Launch,
    Control(String, RunControl),
    Arm(Armed),
    Confirm,
    Done(Result<String, String>),
    ShowMonitor(String),
    Feed(Vec<BusMessage>),
    Cfg(crate::config_panel::CfgMsg),
    // Agent spawn form
    OpenSpawnAgent,
    CloseSpawnAgent,
    SpawnAgentRole(Choice),
    SpawnAgentWorkspace(Choice),
    SpawnAgentVendor(Choice),
    SpawnAgentModel(String),
    SpawnAgentTask(String),
    SpawnAgentWorktree(bool),
    SpawnAgent,
    // Agent actions
    KillAgent(String),
    ResumeAgents,
    AssignAgentTask(String, Option<u32>),
    // Task form
    TaskNew,
    TaskEditOpen(u32),
    TaskTitle(String),
    TaskDescription(text_editor::Action),
    TaskCriteria(text_editor::Action),
    TaskSpec(text_editor::Action),
    TaskSave,
    TaskCancel,
    /// Open the agent form for this task.
    RunTaskWithAgent(u32),
}

/// The create / edit form of a task.
pub struct TaskForm {
    /// The task being edited; `None` when it is a new one.
    editing: Option<u32>,
    title: String,
    description: text_editor::Content,
    criteria: text_editor::Content,
    spec: text_editor::Content,
}

impl TaskForm {
    fn new() -> Self {
        Self {
            editing: None,
            title: String::new(),
            description: text_editor::Content::new(),
            criteria: text_editor::Content::new(),
            spec: text_editor::Content::new(),
        }
    }

    fn of(task: &shepherd_app_core::data::TaskRow) -> Self {
        Self {
            editing: Some(task.status.id),
            title: task.status.title.clone(),
            description: text_editor::Content::with_text(&task.issue_body),
            criteria: text_editor::Content::with_text(&shepherd_app_core::data::criteria_text(
                task,
            )),
            spec: text_editor::Content::with_text(&task.spec),
        }
    }
}

pub struct Panel {
    pub tab: Tab,
    run: Option<String>,
    agent: Option<String>,
    task: Option<u32>,
    pattern_sel: Option<String>,
    edit: Option<PatternEditor>,
    launching: bool,
    launch_task: Option<u32>,
    pattern: Option<Choice>,
    objective: text_editor::Content,
    armed: Option<Armed>,
    pub notice: String,
    monitor: Option<String>,
    feed: Vec<BusMessage>,
    cfg: crate::config_panel::ConfigPanel,
    /// The tab the window starts on loads once, when the first data arrives.
    cfg_started: bool,
    // Spawn agent form
    spawning_agent: bool,
    spawn_role: Option<Choice>,
    spawn_workspace: Option<Choice>,
    spawn_vendor: Option<Choice>,
    spawn_model: String,
    spawn_task_text: String,
    spawn_worktree: bool,
    /// The task an agent is being launched for (set by "Run with an agent").
    spawn_task_id: Option<u32>,
    /// The form to create or edit a task, while it is open.
    task_form: Option<TaskForm>,
}

/// What the panel needs from the app to act.
pub struct Ctx<'a> {
    pub api: PathBuf,
    pub data: &'a Data,
    pub snapshot: &'a Snapshot,
    /// Animation step of the prism canvases.
    pub frame: u32,
}

impl Default for Panel {
    /// `SHEPHERD_NATIVE_TAB=agents|tasks|monitor` and `SHEPHERD_NATIVE_LAUNCH=1` pick what the window starts
    /// on; they exist to take pictures of each view (`make native-shot`).
    fn default() -> Self {
        let tab = match std::env::var("SHEPHERD_NATIVE_TAB").as_deref() {
            Ok("overview") => Tab::Overview,
            Ok("agents") => Tab::Agents,
            Ok("tasks") => Tab::Tasks,
            Ok("patterns") => Tab::Patterns,
            Ok("monitor") => Tab::Monitor,
            Ok("skills") => Tab::Skills,
            Ok("git") => Tab::Git,
            Ok("events") => Tab::Events,
            Ok("models") => Tab::Models,
            _ => Tab::Runs,
        };
        Self {
            launching: std::env::var_os("SHEPHERD_NATIVE_LAUNCH").is_some(),
            tab,
            run: None,
            agent: None,
            task: None,
            pattern_sel: None,
            edit: None,
            launch_task: None,
            pattern: None,
            objective: text_editor::Content::new(),
            armed: None,
            notice: String::new(),
            monitor: None,
            feed: Vec::new(),
            cfg: Default::default(),
            cfg_started: false,
            spawning_agent: false,
            spawn_role: None,
            spawn_workspace: None,
            spawn_vendor: None,
            spawn_model: String::new(),
            spawn_task_text: String::new(),
            spawn_worktree: false,
            spawn_task_id: None,
            // picture aid: `SHEPHERD_NATIVE_TASKFORM=new` opens the new-task form (`edit:N` waits for the data)
            task_form: (std::env::var("SHEPHERD_NATIVE_TASKFORM").as_deref() == Ok("new"))
                .then(TaskForm::new),
        }
    }
}

fn state_badge(state: PatternRunState) -> (&'static str, Color) {
    match state {
        PatternRunState::Running => ("running", OK),
        PatternRunState::Paused | PatternRunState::Stalled | PatternRunState::Degraded => (
            match state {
                PatternRunState::Paused => "paused",
                PatternRunState::Stalled => "stalled",
                _ => "degraded",
            },
            WARN,
        ),
        PatternRunState::Failed => ("failed", BAD),
        PatternRunState::Done => ("done", DIM),
        PatternRunState::Stopped => ("stopped", DIM),
    }
}

fn agent_status(state: &str) -> Status {
    match state {
        "working" | "starting" => Status::Working,
        "blocked" => Status::Blocked,
        "done" => Status::Done,
        "idle" => Status::Idle,
        _ => Status::Unknown,
    }
}

fn task_state_color(state: &str) -> Color {
    match state {
        "done" | "completed" | "archived" => OK,
        "blocked" | "waiting" => WARN,
        "working" | "in_progress" | "running" => ACCENT,
        _ => DIM,
    }
}

/// One KPI cell (`.kpi`): pixel label, big value, optional muted subtitle.
pub(crate) fn kpi_cell<'a>(
    label: &'static str,
    value: String,
    color: Color,
    sub: Option<String>,
) -> Element<'a, Message> {
    kpi_inner(label, value, color, sub, None)
}

/// A KPI cell with the animated sparkline on its right.
fn kpi_cell_spark<'a>(
    label: &'static str,
    value: String,
    color: Color,
    sub: Option<String>,
    frame: u32,
) -> Element<'a, Message> {
    kpi_inner(label, value, color, sub, Some(frame))
}

fn kpi_inner<'a>(
    label: &'static str,
    value: String,
    color: Color,
    sub: Option<String>,
    spark: Option<u32>,
) -> Element<'a, Message> {
    let mut body = column![
        theme::label(label),
        text(value).font(theme::MONO_BOLD).size(30).color(color),
    ]
    .spacing(4);
    if let Some(sub) = sub {
        body = body.push(text(sub).size(11).color(DIM));
    }
    let content: Element<'a, Message> = match spark {
        Some(frame) => row![
            body,
            container(text("")).width(Length::Fill),
            Canvas::new(Prism {
                preset: Preset::SPARK,
                frame,
            })
            .width(120)
            .height(26),
        ]
        .align_y(iced::Alignment::End)
        .into(),
        None => body.into(),
    };
    container(content)
        .width(Length::Fill)
        .padding([12, 16])
        .style(kpi_cell_style)
        .into()
}

fn agent_metric<'a>(label: &'static str, value: usize, color: Color) -> Element<'a, Message> {
    kpi_cell(label, value.to_string(), color, None)
}

fn agent_metric_spark<'a>(
    label: &'static str,
    value: usize,
    color: Color,
    frame: u32,
) -> Element<'a, Message> {
    kpi_cell_spark(label, value.to_string(), color, None, frame)
}

/// `.sq`: 18 px square with the state glyph, filled with the state color.
pub(crate) fn agent_square<'a>(status: Status) -> Element<'a, Message> {
    let color = status_color(status);
    container(
        text(status_glyph(status))
            .font(theme::MONO_BOLD)
            .size(10)
            .color(theme::BG),
    )
    .center(18)
    .style(move |_| iced::widget::container::Style {
        background: Some(Background::Color(color)),
        border: iced::Border {
            radius: 3.0.into(),
            ..iced::Border::default()
        },
        ..iced::widget::container::Style::default()
    })
    .into()
}

/// `.badge`: small solid pill.
pub(crate) fn badge<'a>(label: impl ToString, color: Color) -> Element<'a, Message> {
    container(
        text(label.to_string())
            .font(theme::MONO_BOLD)
            .size(10.5)
            .color(theme::BG),
    )
    .padding([2, 7])
    .style(theme::badge_style(color))
    .into()
}

/// `.btn`-family button body: 34 px high, label centered.
fn btn_body<'a>(label: impl ToString) -> Element<'a, Message> {
    container(text(label.to_string()).size(13))
        .center_y(Length::Fill)
        .into()
}

pub(crate) fn btn<'a>(label: impl ToString, msg: PanelMsg) -> Element<'a, Message> {
    button(btn_body(label))
        .padding([0, 12])
        .height(34)
        .style(theme::outline_button_style(false))
        .on_press(Message::Panel(msg))
        .into()
}

pub(crate) fn btn_ghost<'a>(label: impl ToString, msg: PanelMsg) -> Element<'a, Message> {
    button(btn_body(label))
        .padding([0, 12])
        .height(34)
        .style(theme::outline_button_style(true))
        .on_press(Message::Panel(msg))
        .into()
}

pub(crate) fn btn_link<'a>(label: impl ToString, msg: PanelMsg) -> Element<'a, Message> {
    button(btn_body(label))
        .padding([0, 8])
        .height(34)
        .style(theme::link_button_style())
        .on_press(Message::Panel(msg))
        .into()
}

pub(crate) fn btn_primary<'a>(label: impl ToString, msg: PanelMsg) -> Element<'a, Message> {
    button(btn_body(label))
        .padding([0, 12])
        .height(34)
        .style(primary_button_style())
        .on_press(Message::Panel(msg))
        .into()
}

/// `.card`: optional pixel-font title with something on the right, a hairline, then the body.
pub(crate) fn card<'a>(
    title: &'static str,
    right: Option<Element<'a, Message>>,
    body: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut head = row![theme::label(title), container(text("")).width(Length::Fill)]
        .align_y(iced::Alignment::Center);
    if let Some(right) = right {
        head = head.push(right);
    }
    container(column![
        container(head).padding([10, 14]).width(Length::Fill),
        container(text(""))
            .width(Length::Fill)
            .height(1)
            .style(|_| iced::widget::container::Style {
                background: Some(Background::Color(LINE)),
                ..iced::widget::container::Style::default()
            }),
        container(body).padding([12, 14]).width(Length::Fill),
    ])
    .width(Length::Fill)
    .style(card_style)
    .into()
}

/// One message of the communication feed (`.msg`): time, emitter (each with its own colour), a tag (topic or
/// kind, coloured by what it means) and the text. `with_position` adds the partition and offset (Events).
pub(crate) fn message_row<'a>(m: &BusMessage, with_position: bool) -> Element<'a, Message> {
    let time: String = m.ts.replace('T', " ").chars().skip(11).take(8).collect();
    let time = if time.is_empty() {
        "--:--:--".to_owned()
    } else {
        time
    };
    let (_, body) = feed_line(m);
    let tag = if m.topic.is_empty() {
        m.kind.clone()
    } else {
        format!("#{}", m.topic)
    };
    let tag_color = theme::kind_color(&m.kind, !m.topic.is_empty());
    let who_color = theme::emitter_color(&m.from);
    let mut who = row![
        container(text("").width(8).height(8)).style(move |_| iced::widget::container::Style {
            background: Some(Background::Color(who_color)),
            border: iced::Border {
                radius: 2.0.into(),
                ..iced::Border::default()
            },
            ..iced::widget::container::Style::default()
        }),
        text(m.from.clone()).size(13).color(who_color),
        container(text(tag).size(11).color(tag_color))
            .padding([0, 6])
            .style(move |_| iced::widget::container::Style {
                border: iced::Border {
                    color: tag_color,
                    width: 1.0,
                    radius: 3.0.into(),
                },
                ..iced::widget::container::Style::default()
            }),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    if !m.topic.is_empty() && !m.kind.is_empty() && m.kind != "msg" {
        who = who.push(text(m.kind.clone()).size(11).color(tag_color));
    }
    if !m.to.is_empty() {
        who = who.push(text(format!("→ {}", m.to)).size(12).color(DIM));
    }
    if with_position {
        who = who.push(
            text(format!("p{} · #{}", m.partition, m.offset))
                .size(11)
                .color(DIM),
        );
    }
    column![
        container(
            row![
                text(time).size(13).color(DIM).width(80),
                column![who, text(body).size(13).color(theme::FG)].spacing(2),
            ]
            .spacing(12),
        )
        .padding([10, 0]),
        container(text(""))
            .width(Length::Fill)
            .height(1)
            .style(|_| iced::widget::container::Style {
                background: Some(Background::Color(LINE)),
                ..iced::widget::container::Style::default()
            }),
    ]
    .into()
}

/// `.alert`: tinted box for a stalled run or an error.
pub(crate) fn alert<'a>(content: Element<'a, Message>, color: Color) -> Element<'a, Message> {
    container(content)
        .padding([10, 14])
        .width(Length::Fill)
        .style(theme::alert_style(color))
        .into()
}

impl Panel {
    pub fn update(&mut self, message: PanelMsg, ctx: &Ctx<'_>) -> Task<Message> {
        self.cfg.set_api(&ctx.api);
        // any action other than arming disarms
        if !matches!(message, PanelMsg::Arm(_) | PanelMsg::Confirm) {
            self.armed = None;
        }
        let api = ctx.api.clone();
        let root = ctx.data.root.display().to_string();
        match message {
            PanelMsg::Tab(tab) => {
                self.tab = tab;
                if let Some(task) = self.load_config_tab(tab, ctx) {
                    return task;
                }
                if tab == Tab::Monitor {
                    return self.load_feed(ctx);
                }
                if tab == Tab::Patterns && self.edit.is_none() {
                    if let Some(first) = self
                        .pattern_sel
                        .clone()
                        .or_else(|| ctx.data.patterns.first().map(|p| p.name.clone()))
                    {
                        return self.update(PanelMsg::SelectPattern(first), ctx);
                    }
                }
            }
            PanelMsg::Cfg(msg) => return self.cfg.update(msg, &ctx.data.root),
            PanelMsg::SelectRun(id) => {
                self.tab = Tab::Runs;
                self.run = Some(id);
            }
            PanelMsg::SelectAgent(id) => {
                self.tab = Tab::Agents;
                self.agent = Some(id);
                self.monitor = None;
                return self.load_feed(ctx);
            }
            PanelMsg::SelectTask(id) => {
                self.tab = Tab::Tasks;
                self.task = Some(id);
                self.task_form = None;
            }
            PanelMsg::SelectPattern(name) => {
                self.pattern_sel = Some(name.clone());
                self.edit = None;
                return Task::perform(
                    async move { pattern_graph::load(std::path::Path::new(&root), &name) },
                    |r| Message::Panel(PanelMsg::PatternLoaded(r)),
                );
            }
            PanelMsg::PickAndLaunch(name) => {
                self.pattern = Some(Choice {
                    name: name.clone(),
                    label: name,
                });
                return self.update(PanelMsg::OpenLauncher(None), ctx);
            }
            PanelMsg::PatternLoaded(Ok(view)) => self.edit = Some(PatternEditor::from_view(view)),
            PanelMsg::EditPattern(msg) => {
                let saved = matches!(msg, EditMsg::Save);
                if let Some(edit) = self.edit.as_mut() {
                    if let Some(note) = edit.update(msg, &ctx.data.root) {
                        self.notice = note;
                    }
                    if saved {
                        self.pattern_sel = Some(edit.editor.pattern.name.clone());
                        return Task::done(Message::Reload);
                    }
                }
            }
            PanelMsg::PatternLoaded(Err(e)) => self.notice = e,
            PanelMsg::OpenLauncher(task) => {
                self.tab = Tab::Runs;
                self.launching = true;
                self.launch_task = task;
                let prompt = task
                    .and_then(|id| ctx.data.task(id))
                    .map(task_prompt)
                    .unwrap_or_default();
                self.objective = text_editor::Content::with_text(&prompt);
                if self.pattern.is_none() {
                    self.pattern = ctx.data.patterns.first().map(|p| Choice {
                        name: p.name.clone(),
                        label: p.name.clone(),
                    });
                }
            }
            PanelMsg::CloseLauncher => self.launching = false,
            PanelMsg::PickPattern(choice) => self.pattern = Some(choice),
            PanelMsg::Edit(action) => self.objective.perform(action),
            PanelMsg::Launch => {
                let first = ctx.data.patterns.first().map(|p| Choice {
                    name: p.name.clone(),
                    label: p.name.clone(),
                });
                let Some(pattern) = self.pattern.clone().or(first) else {
                    self.notice = "Pick a pattern first.".into();
                    return Task::none();
                };
                let text = self.objective.text();
                let objective = match self.launch_task.and_then(|id| ctx.data.task(id)) {
                    Some(task) => Some(ensure_task_prefix(&text, task)),
                    None => Some(text.trim().to_owned()).filter(|t| !t.is_empty()),
                };
                let workspace = ctx.snapshot.focused_workspace_id.clone();
                self.notice = format!("Starting {}…", pattern.name);
                return Task::perform(
                    async move {
                        actions::start_pattern_run(
                            &api,
                            &pattern.name,
                            objective.as_deref(),
                            Some(&root),
                            workspace.as_deref(),
                        )
                        .map(|launched| {
                            let agents = launched.run["agents"].as_array().map_or(0, Vec::len);
                            format!(
                                "Run {} started with {agents} agent(s){}",
                                launched.run["id"].as_str().unwrap_or("?"),
                                if launched.over_limit {
                                    " (over the concurrent-agent limit)"
                                } else {
                                    ""
                                }
                            )
                        })
                    },
                    |r| Message::Panel(PanelMsg::Done(r)),
                );
            }
            PanelMsg::Control(id, control) => {
                if control == RunControl::Stop && self.armed != Some(Armed::StopRun(id.clone())) {
                    self.armed = Some(Armed::StopRun(id));
                    return Task::none();
                }
                self.armed = None;
                return Task::perform(
                    async move {
                        actions::control_run(&api, &id, control, Some(&root))
                            .map(|_| format!("{control:?}: ok"))
                    },
                    |r| Message::Panel(PanelMsg::Done(r)),
                );
            }
            PanelMsg::Arm(what) => self.armed = Some(what),
            PanelMsg::Confirm => {
                let Some(armed) = self.armed.take() else {
                    return Task::none();
                };
                match armed {
                    Armed::StopRun(id) => {
                        return self.update(PanelMsg::Control(id, RunControl::Stop), ctx)
                    }
                    Armed::DeleteTask(id) => {
                        if self.task == Some(id) {
                            self.task = None;
                        }
                        return Task::perform(
                            async move {
                                actions::delete_task(&api, id, &root)
                                    .map(|_| format!("Deleted task #{id}"))
                            },
                            |r| Message::Panel(PanelMsg::Done(r)),
                        );
                    }
                    Armed::DeleteRun(id) => {
                        if self.run.as_deref() == Some(id.as_str()) {
                            self.run = None;
                        }
                        return Task::perform(
                            async move {
                                actions::delete_run(&api, &id, Some(&root), true).map(|agents| {
                                    format!("Deleted run {id} and cleaned {agents} agent(s)")
                                })
                            },
                            |r| Message::Panel(PanelMsg::Done(r)),
                        );
                    }
                    Armed::CleanRuns => {
                        return Task::perform(
                            async move {
                                actions::clean_runs(&api, Some(&root)).map(|(runs, agents)| {
                                    format!("Cleaned {runs} run(s) and {agents} agent(s)")
                                })
                            },
                            |r| Message::Panel(PanelMsg::Done(r)),
                        );
                    }
                    Armed::CleanBus | Armed::CleanBusForce => {
                        let force = armed == Armed::CleanBusForce;
                        return Task::perform(
                            async move {
                                actions::clean_bus(&api, force)
                                    .map(|n| format!("Cleared {n} message(s)"))
                            },
                            |r| match r {
                                Err(e) if e.contains("confirmation_required") => {
                                    Message::Panel(PanelMsg::Arm(Armed::CleanBusForce))
                                }
                                other => Message::Panel(PanelMsg::Done(other)),
                            },
                        );
                    }
                    Armed::ArchiveDone => {
                        let done: Vec<u32> = ctx
                            .data
                            .tasks
                            .iter()
                            .filter(|t| {
                                matches!(
                                    t.status.state.to_lowercase().as_str(),
                                    "done" | "completed"
                                )
                            })
                            .map(|t| t.status.id)
                            .collect();
                        return Task::perform(
                            async move {
                                let archived = done
                                    .iter()
                                    .filter(|id| {
                                        actions::archive_task(&api, **id, Some(&root)).is_ok()
                                    })
                                    .count();
                                Ok(format!("Archived {archived} task(s)"))
                            },
                            |r| Message::Panel(PanelMsg::Done(r)),
                        );
                    }
                }
            }
            PanelMsg::Done(Ok(note)) => {
                self.notice = note;
                self.launching = false;
                self.task_form = None;
                return Task::done(Message::Reload);
            }
            PanelMsg::Done(Err(e)) => self.notice = e,
            PanelMsg::ShowMonitor(run) => {
                self.tab = Tab::Monitor;
                self.monitor = Some(run);
                return self.load_feed(ctx);
            }
            PanelMsg::Feed(feed) => self.feed = feed,
            // Spawn agent form
            PanelMsg::OpenSpawnAgent => {
                self.spawning_agent = true;
                self.spawn_role = None;
                self.spawn_workspace = None;
                self.spawn_vendor = None;
                self.spawn_model.clear();
                self.spawn_task_text.clear();
                self.spawn_task_id = None;
                self.spawn_worktree = false;
                // Pre-select first role and workspace
                if let Some(first_role) = ctx.data.models.roles.iter().next() {
                    self.spawn_role = Some(Choice {
                        name: first_role.0.clone(),
                        label: first_role.0.clone(),
                    });
                }
                if let Some(first_ws) = ctx.snapshot.workspaces.first() {
                    self.spawn_workspace = Some(Choice {
                        name: first_ws.workspace_id.clone(),
                        label: first_ws.label.clone(),
                    });
                }
            }
            PanelMsg::CloseSpawnAgent => self.spawning_agent = false,
            PanelMsg::SpawnAgentRole(choice) => {
                self.spawn_role = Some(choice.clone());
                // Auto-select vendor from role
                if let Some(role) = ctx.data.models.roles.get(&choice.name) {
                    if !role.vendor.is_empty() {
                        self.spawn_vendor = Some(Choice {
                            name: role.vendor.clone(),
                            label: role.vendor.clone(),
                        });
                    }
                }
            }
            PanelMsg::SpawnAgentWorkspace(choice) => self.spawn_workspace = Some(choice),
            PanelMsg::SpawnAgentVendor(choice) => self.spawn_vendor = Some(choice),
            PanelMsg::SpawnAgentModel(model) => self.spawn_model = model,
            PanelMsg::SpawnAgentTask(text) => self.spawn_task_text = text,
            PanelMsg::SpawnAgentWorktree(on) => self.spawn_worktree = on,
            PanelMsg::SpawnAgent => {
                let Some(role) = self.spawn_role.clone() else {
                    self.notice = "Pick a role first.".into();
                    return Task::none();
                };
                let task_text = self.spawn_task_text.trim().to_owned();
                if task_text.is_empty() {
                    self.notice = "Enter a task description.".into();
                    return Task::none();
                }
                let workspace = self.spawn_workspace.as_ref().map(|c| c.name.clone());
                let vendor = self.spawn_vendor.as_ref().map(|c| c.name.clone());
                let model = if self.spawn_model.is_empty() {
                    None
                } else {
                    Some(self.spawn_model.clone())
                };
                let root = ctx.data.root.display().to_string();
                let worktree = self.spawn_worktree;
                let task_id = self.spawn_task_id;
                self.notice = format!("Starting {} agent…", role.name);
                return Task::perform(
                    async move {
                        actions::spawn_agent(
                            &api,
                            &role.name,
                            &task_text,
                            Some(&root),
                            workspace.as_deref(),
                            vendor.as_deref(),
                            model.as_deref(),
                            worktree,
                            task_id,
                        )
                        .map(|agent| {
                            format!("Agent {} started", agent["id"].as_str().unwrap_or("?"))
                        })
                    },
                    |r| Message::Panel(PanelMsg::Done(r)),
                );
            }
            // Task form
            PanelMsg::TaskNew => {
                self.launching = false;
                self.task_form = Some(TaskForm::new());
            }
            PanelMsg::TaskEditOpen(id) => {
                if let Some(task) = ctx.data.task(id) {
                    self.launching = false;
                    self.task_form = Some(TaskForm::of(task));
                }
            }
            PanelMsg::TaskTitle(title) => {
                if let Some(form) = &mut self.task_form {
                    form.title = title;
                }
            }
            PanelMsg::TaskDescription(action) => {
                if let Some(form) = &mut self.task_form {
                    form.description.perform(action);
                }
            }
            PanelMsg::TaskCriteria(action) => {
                if let Some(form) = &mut self.task_form {
                    form.criteria.perform(action);
                }
            }
            PanelMsg::TaskSpec(action) => {
                if let Some(form) = &mut self.task_form {
                    form.spec.perform(action);
                }
            }
            PanelMsg::TaskCancel => self.task_form = None,
            PanelMsg::TaskSave => {
                let Some(form) = &self.task_form else {
                    return Task::none();
                };
                let title = form.title.trim().to_owned();
                if title.is_empty() {
                    self.notice = "A task needs a title.".into();
                    return Task::none();
                }
                let description = form.description.text().trim().to_owned();
                let criteria = shepherd_app_core::data::parse_criteria(&form.criteria.text());
                let spec = form.spec.text();
                let editing = form.editing;
                let root = ctx.data.root.display().to_string();
                return Task::perform(
                    async move {
                        match editing {
                            None => {
                                actions::create_task(&api, &root, &title, &description, &criteria)
                                    .map(|id| format!("Created task #{id}"))
                            }
                            Some(id) => actions::update_task(
                                &api,
                                &root,
                                id,
                                &title,
                                &description,
                                Some(&spec),
                                &criteria,
                            )
                            .map(|_| format!("Saved task #{id}")),
                        }
                    },
                    |r| Message::Panel(PanelMsg::Done(r)),
                );
            }
            PanelMsg::RunTaskWithAgent(id) => {
                let Some(task) = ctx.data.task(id) else {
                    return Task::none();
                };
                let prompt = task_prompt(task);
                self.tab = Tab::Agents;
                let opened = self.update(PanelMsg::OpenSpawnAgent, ctx);
                self.spawn_task_text = prompt;
                self.spawn_task_id = Some(id);
                return opened;
            }
            // Agent actions
            PanelMsg::KillAgent(id) => {
                return Task::perform(
                    async move {
                        actions::kill_and_clean_agent(&api, &id)
                            .map(|_| format!("Agent {} cleaned", id))
                    },
                    |r| Message::Panel(PanelMsg::Done(r)),
                );
            }
            PanelMsg::ResumeAgents => {
                return Task::perform(
                    async move { actions::resume_agents(&api).map(|_| "Resumed idle agents".into()) },
                    |r| Message::Panel(PanelMsg::Done(r)),
                );
            }
            PanelMsg::AssignAgentTask(agent_id, task_id) => {
                let root = ctx.data.root.display().to_string();
                return Task::perform(
                    async move {
                        actions::assign_agent_task(&api, &agent_id, task_id, &root)
                            .map(|_| format!("Agent {} task updated", agent_id))
                    },
                    |r| Message::Panel(PanelMsg::Done(r)),
                );
            }
        }
        Task::none()
    }

    /// After the data (re)loads: the Patterns tab opens on its first pattern when none was picked yet.
    /// The tabs that read slow things (git, the bus, skills) load when opened, off the UI thread.
    pub fn load_config_tab(&mut self, tab: Tab, ctx: &Ctx<'_>) -> Option<Task<Message>> {
        self.cfg.set_api(&ctx.api);
        use crate::config_panel::ConfigPanel as C;
        let root = &ctx.data.root;
        match tab {
            Tab::Skills => {
                self.cfg.loading = true;
                Some(C::load_skills(root))
            }
            Tab::Git => {
                self.cfg.loading = true;
                Some(C::load_git(root))
            }
            Tab::Events => {
                self.cfg.loading = true;
                Some(C::load_topics(&ctx.api))
            }
            Tab::Models => {
                self.cfg.loading = true;
                Some(C::load_models(root))
            }
            _ => None,
        }
    }

    pub fn on_data(&mut self, ctx: &Ctx<'_>) -> Task<Message> {
        self.cfg.set_api(&ctx.api);
        if let Some(id) = std::env::var("SHEPHERD_NATIVE_TASKFORM")
            .ok()
            .and_then(|v| v.strip_prefix("edit:").and_then(|n| n.parse::<u32>().ok()))
        {
            if self.task_form.is_none() && ctx.data.task(id).is_some() {
                let _ = self.update(PanelMsg::TaskEditOpen(id), ctx);
            }
        }
        if !std::mem::replace(&mut self.cfg_started, true) {
            let counts = crate::config_panel::ConfigPanel::load_counts(&ctx.data.root, &ctx.api);
            return match self.load_config_tab(self.tab, ctx) {
                Some(task) => Task::batch([task, counts]),
                None => counts,
            };
        }
        if self.tab == Tab::Patterns && self.pattern_sel.is_none() {
            if let Some(first) = ctx.data.patterns.first().map(|p| p.name.clone()) {
                return self.update(PanelMsg::SelectPattern(first), ctx);
            }
        }
        Task::none()
    }

    /// Reloads the communication of the monitored run (called when files change, too).
    pub fn load_feed(&self, ctx: &Ctx<'_>) -> Task<Message> {
        let agents: Vec<String> = match self
            .monitor
            .as_deref()
            .and_then(|id| ctx.data.runs.iter().find(|r| r.run.id == id))
        {
            Some(row) => row.run.agents.iter().map(|a| a.id.clone()).collect(),
            None => self
                .agent
                .as_deref()
                .map(|id| vec![id.to_owned()])
                .unwrap_or_else(|| ctx.data.agents.iter().map(|a| a.id.clone()).collect()),
        };
        Task::perform(async move { bus_feed(&agents, 300) }, |feed| {
            Message::Panel(PanelMsg::Feed(feed))
        })
    }

    fn arm_button<'a>(
        &self,
        key: Armed,
        label: &'a str,
        armed_label: &'a str,
    ) -> Element<'a, Message> {
        let armed = self.armed.as_ref() == Some(&key);
        let msg = if armed {
            PanelMsg::Confirm
        } else {
            PanelMsg::Arm(key)
        };
        let btn_label = if armed { armed_label } else { label };
        let content = btn_body(btn_label);
        if armed {
            button(content)
                .padding([0, 12])
                .height(34)
                .style(danger_button_style())
        } else {
            button(content)
                .padding([0, 12])
                .height(34)
                .style(theme::outline_button_style(false))
        }
        .on_press(Message::Panel(msg))
        .into()
    }

    /// Section tabs (`.tabs`): a pixel-font group label, then its tabs; a count badge turns orange
    /// when something in that section needs the user.
    fn section_tabs<'a>(&self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let runs_warn = ctx.data.runs.iter().any(|r| {
            matches!(
                r.run.state,
                PatternRunState::Stalled | PatternRunState::Degraded | PatternRunState::Failed
            )
        });
        let agents_warn = ctx
            .data
            .agents
            .iter()
            .any(|a| a.state == "blocked" || !a.notice.is_empty());
        let counts = self.cfg.counts();
        let tab_button = |tab: Tab, name: &'static str| {
            let (count, warn) = match tab {
                Tab::Runs => (Some(ctx.data.runs.len()), runs_warn),
                Tab::Agents => (Some(ctx.data.agents.len()), agents_warn),
                Tab::Tasks => (Some(ctx.data.tasks.len()), false),
                Tab::Patterns => (Some(ctx.data.patterns.len()), false),
                Tab::Skills => (Some(counts.0), false),
                Tab::Events => (Some(counts.1), false),
                Tab::Models => (Some(counts.2), false),
                _ => (None, false),
            };
            let selected = self.tab == tab;
            let mut content = row![text(name).size(13)]
                .spacing(8)
                .align_y(iced::Alignment::Center);
            if let Some(count) = count {
                content = content.push(
                    container(text(count.to_string()).size(10.5))
                        .padding([0, 5])
                        .style(move |_| theme::count_style(selected, warn)),
                );
            }
            let tab_btn = button(container(content).padding([0, 4]).center_y(Length::Fill))
                .padding([0, 4])
                .height(34)
                .style(item_style(selected))
                .on_press(Message::Panel(PanelMsg::Tab(tab)));
            if selected {
                // the underline is a layer over the tab, so it takes the tab's width instead of widening it
                iced::widget::stack![
                    tab_btn,
                    container(
                        Canvas::new(Prism {
                            preset: Preset::TABBAR,
                            frame: ctx.frame,
                        })
                        .width(Length::Fill)
                        .height(2)
                    )
                    .padding([0, 4])
                    .align_bottom(Length::Fill),
                ]
                .into()
            } else {
                Element::from(tab_btn)
            }
        };
        let group = |name: &'static str, tabs: &[(Tab, &'static str)]| {
            let mut r = row![container(theme::label(name)).padding(pad(0.0, 10.0, 0.0, 4.0))]
                .spacing(2)
                .align_y(iced::Alignment::Center);
            for (tab, label) in tabs {
                r = r.push(tab_button(*tab, label));
            }
            r
        };
        let separator =
            container(text(""))
                .width(1)
                .height(22)
                .style(|_| iced::widget::container::Style {
                    background: Some(Background::Color(theme::LINE2)),
                    ..iced::widget::container::Style::default()
                });
        container(
            row![
                group(
                    "WORKSPACE",
                    &[
                        (Tab::Overview, "Overview"),
                        (Tab::Runs, "Runs"),
                        (Tab::Agents, "Agents"),
                        (Tab::Tasks, "Tasks"),
                        (Tab::Patterns, "Patterns"),
                    ]
                ),
                container(separator).padding([0, 12]),
                group(
                    "INSPECT",
                    &[
                        (Tab::Monitor, "Activity"),
                        (Tab::Skills, "Skills"),
                        (Tab::Git, "Git"),
                        (Tab::Events, "Events"),
                        (Tab::Models, "Models"),
                    ]
                ),
            ]
            .align_y(iced::Alignment::Center)
            .wrap(),
        )
        .padding([8, 14])
        .width(Length::Fill)
        .style(theme::bottom_line_style)
        .into()
    }

    pub fn view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let tabs = self.section_tabs(ctx);
        let body = match self.tab {
            Tab::Overview => self.overview_view(ctx),
            Tab::Runs => self.runs_view(ctx),
            Tab::Agents => self.agents_view(ctx),
            Tab::Tasks => self.tasks_view(ctx),
            Tab::Patterns => self.patterns_view(ctx),
            Tab::Monitor => self.monitor_view(ctx),
            Tab::Skills => self.cfg.skills_view(),
            Tab::Git => self.cfg.git_view(),
            Tab::Events => self.cfg.events_view(ctx.frame),
            Tab::Models => self.cfg.models_view(),
        };
        column![tabs, body].into()
    }

    /// The window now looks at another session: forget what was picked and loaded from the previous one.
    pub fn session_changed(&mut self) {
        self.run = None;
        self.agent = None;
        self.task = None;
        self.pattern_sel = None;
        self.edit = None;
        self.launching = false;
        self.launch_task = None;
        self.armed = None;
        self.monitor = None;
        self.feed.clear();
        self.spawning_agent = false;
        self.notice.clear();
        self.cfg = crate::config_panel::ConfigPanel::default();
        self.cfg_started = false;
    }

    /// Events stay live: when the state files change, the topics and the open topic's messages are read again.
    pub fn refresh_events(&self) -> Task<Message> {
        self.cfg.reload_events()
    }

    /// The global config file shown at the right of the status bar, once it is known.
    pub fn config_path(&self) -> Option<String> {
        self.cfg.config_path()
    }

    /// Status line text for the window's status bar (`#sbarMsg`).
    pub fn notice<'a>(&'a self) -> Element<'a, Message> {
        let notice_text = if self.cfg.notice.is_empty()
            || matches!(
                self.tab,
                Tab::Overview | Tab::Runs | Tab::Agents | Tab::Tasks | Tab::Patterns | Tab::Monitor
            ) {
            self.notice.clone()
        } else {
            self.cfg.notice.clone()
        };
        let notice_color = if notice_text.contains("error")
            || notice_text.contains("failed")
            || notice_text.contains("✖")
        {
            BAD
        } else if notice_text.contains("Starting")
            || notice_text.contains("Loading")
            || notice_text.contains("confirm")
        {
            WARN
        } else if notice_text.is_empty() {
            DIM
        } else {
            OK
        };
        let notice: Element<'a, Message> = if notice_text.is_empty() {
            text("Ready").size(11.5).color(DIM).into()
        } else {
            container(
                row![
                    text(if notice_color == BAD { "✖" } else { "●" })
                        .size(11)
                        .color(notice_color),
                    text(notice_text).size(11).color(notice_color),
                ]
                .spacing(7),
            )
            .into()
        };
        notice
    }

    fn list<'a>(
        &self,
        items: Vec<Element<'a, Message>>,
        footer: Vec<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        list_box(items, footer)
    }

    fn detail<'a>(&self, content: Element<'a, Message>) -> Element<'a, Message> {
        detail_box(content)
    }

    fn runs_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let active = ctx
            .data
            .runs
            .iter()
            .filter(|r| {
                matches!(
                    r.run.state,
                    PatternRunState::Running
                        | PatternRunState::Paused
                        | PatternRunState::Stalled
                        | PatternRunState::Degraded
                )
            })
            .count();
        let healthy = ctx
            .data
            .runs
            .iter()
            .filter(|r| {
                matches!(
                    r.run.state,
                    PatternRunState::Running | PatternRunState::Done
                )
            })
            .count();
        let attention = ctx
            .data
            .runs
            .iter()
            .filter(|r| {
                matches!(
                    r.run.state,
                    PatternRunState::Stalled | PatternRunState::Degraded | PatternRunState::Failed
                )
            })
            .count();
        let summary = container(
            row![
                agent_metric("TOTAL", ctx.data.runs.len(), FG),
                agent_metric_spark("ACTIVE", active, ACCENT, ctx.frame),
                agent_metric("HEALTHY", healthy, OK),
                agent_metric(
                    "ATTENTION",
                    attention,
                    if attention > 0 { WARN } else { DIM }
                ),
            ]
            .spacing(1),
        )
        .style(kpi_strip_style);
        let items = ctx
            .data
            .runs
            .iter()
            .map(|r| {
                let (label, color) = state_badge(r.run.state);
                let body = column![
                    text(format!(
                        "{} · {}",
                        r.run.pattern,
                        r.run.id.trim_start_matches("run-")
                    ))
                    .size(13)
                    .color(theme::HI),
                    text(format!(
                        "{label} · {}/{} agents",
                        r.live_agents,
                        r.run.agents.len()
                    ))
                    .size(11)
                    .color(color),
                    text(if r.run.task.is_empty() {
                        "No objective".to_owned()
                    } else {
                        r.run.task.chars().take(56).collect()
                    })
                    .size(10)
                    .color(DIM)
                ];
                button(body)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(item_style(
                        self.run
                            .as_deref()
                            .or(ctx.data.runs.first().map(|f| f.run.id.as_str()))
                            == Some(r.run.id.as_str()),
                    ))
                    .on_press(Message::Panel(PanelMsg::SelectRun(r.run.id.clone())))
                    .into()
            })
            .collect();
        let footer = vec![
            btn_link("+ Run pattern", PanelMsg::OpenLauncher(None)),
            self.arm_button(
                Armed::CleanRuns,
                "Clean finished",
                "Confirm: delete finished",
            ),
        ];
        let detail = if self.launching {
            self.launcher_view(ctx)
        } else {
            self.run_detail(ctx)
        };
        column![
            summary,
            row![self.list(items, footer), self.detail(detail)].height(Length::Fill),
        ]
        .spacing(8)
        .into()
    }

    fn overview_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let running = ctx
            .data
            .runs
            .iter()
            .filter(|r| {
                matches!(
                    r.run.state,
                    PatternRunState::Running
                        | PatternRunState::Paused
                        | PatternRunState::Stalled
                        | PatternRunState::Degraded
                )
            })
            .count();
        let active_agents = ctx
            .data
            .agents
            .iter()
            .filter(|a| matches!(a.state.as_str(), "working" | "starting"))
            .count();
        let attention_runs = ctx
            .data
            .runs
            .iter()
            .filter(|r| {
                matches!(
                    r.run.state,
                    PatternRunState::Stalled | PatternRunState::Degraded
                )
            })
            .count();
        let open_tasks = ctx
            .data
            .tasks
            .iter()
            .filter(|t| !matches!(t.status.state.as_str(), "done" | "completed" | "archived"))
            .count();
        let (subtasks_done, subtasks_total) =
            ctx.data
                .tasks
                .iter()
                .fold((0usize, 0usize), |(done, total), task| {
                    let task_total = task.status.subtasks.len();
                    let task_done = task
                        .status
                        .subtasks
                        .iter()
                        .filter(|subtask| matches!(subtask.state.as_str(), "done" | "completed"))
                        .count();
                    (done + task_done, total + task_total)
                });
        let progress_label = if subtasks_total == 0 {
            "No subtasks tracked yet".to_owned()
        } else {
            format!("{subtasks_done}/{subtasks_total} subtasks complete")
        };

        let blocked: Vec<_> = ctx
            .data
            .agents
            .iter()
            .filter(|a| a.state == "blocked" || !a.notice.is_empty())
            .collect();
        let attention_total = blocked.len() + attention_runs;
        let kpis = container(
            row![
                kpi_cell(
                    "RUNS",
                    running.to_string(),
                    ACCENT,
                    Some("active now".into())
                ),
                kpi_cell(
                    "AGENTS",
                    ctx.data.agents.len().to_string(),
                    theme::HI,
                    Some(format!(
                        "{active_agents} working · {} blocked",
                        blocked.len()
                    ))
                ),
                kpi_cell("TASKS", open_tasks.to_string(), WARN, Some("open".into())),
                kpi_cell(
                    "PATTERNS",
                    ctx.data.patterns.len().to_string(),
                    theme::HI,
                    Some("available".into())
                ),
            ]
            .spacing(1),
        )
        .style(kpi_strip_style);

        let attention_card = {
            let right = if attention_total > 0 {
                badge(format!("{} BLOCKED", blocked.len()), WARN)
            } else {
                badge("ALL CLEAR", OK)
            };
            let mut body = column![].spacing(6);
            for a in &blocked {
                body = body.push(
                    row![
                        agent_square(agent_status(&a.state)),
                        text(a.id.clone()).size(13),
                        text(if a.notice.is_empty() {
                            a.state.clone()
                        } else {
                            a.notice.clone()
                        })
                        .size(11)
                        .color(DIM),
                        container(text("")).width(Length::Fill),
                        btn_link("Open →", PanelMsg::SelectAgent(a.id.clone())),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                );
            }
            if attention_runs > 0 {
                body = body.push(
                    text(format!("{attention_runs} run(s) are degraded or stalled."))
                        .size(12)
                        .color(WARN),
                );
            }
            if attention_total == 0 {
                body = body.push(
                    text("Nothing blocked. The flock keeps moving on its own.")
                        .size(13)
                        .color(DIM),
                );
            }
            card("NEEDS ATTENTION", Some(right), body.into())
        };
        let recent_card = {
            let mut body = column![].spacing(10);
            for run in ctx.data.runs.iter().take(5) {
                let (state, color) = state_badge(run.run.state);
                body = body.push(
                    button(
                        column![
                            row![
                                text(run.run.pattern.clone()).size(13).color(theme::HI),
                                text(run.run.id.trim_start_matches("run-").to_owned())
                                    .size(12)
                                    .color(DIM),
                                container(text("")).width(Length::Fill),
                                text(state).size(12).color(color),
                            ]
                            .spacing(10),
                            text(format!(
                                "created {} · {} agents · topics {}",
                                run.run.created_at,
                                run.run.agents.len(),
                                run.run
                                    .topics
                                    .iter()
                                    .map(|t| format!("#{t}"))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            ))
                            .size(11)
                            .color(DIM),
                        ]
                        .spacing(4),
                    )
                    .width(Length::Fill)
                    .padding([4, 6])
                    .style(item_style(false))
                    .on_press(Message::Panel(PanelMsg::SelectRun(run.run.id.clone()))),
                );
            }
            if ctx.data.runs.is_empty() {
                body = body.push(text("No runs yet.").size(13).color(DIM));
            }
            card("RECENT RUNS", None, body.into())
        };

        let pulse = {
            let needs_you = !blocked.is_empty() || attention_runs > 0;
            let message: Element<'a, Message> = if needs_you {
                text(format!("{} agents need you", blocked.len()))
                    .size(12)
                    .color(WARN)
                    .into()
            } else {
                text(format!("{active_agents} agents working"))
                    .size(12)
                    .color(ACCENT)
                    .into()
            };
            container(iced::widget::stack![
                Canvas::new(Prism {
                    preset: Preset::pulse(needs_you),
                    frame: ctx.frame,
                })
                .width(Length::Fill)
                .height(Length::Fill),
                container(
                    container(
                        column![theme::label("HERD PULSE"), message].align_x(iced::Alignment::End)
                    )
                    .padding([6, 10])
                    .style(|_| iced::widget::container::Style {
                        background: Some(Background::Color(Color::from_rgba8(7, 7, 10, 0.8))),
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..iced::Border::default()
                        },
                        ..iced::widget::container::Style::default()
                    }),
                )
                .align_right(Length::Fill)
                .align_bottom(Length::Fill)
                .padding([0, 16]),
            ])
            .height(120)
            .width(Length::Fill)
            .style(|_| iced::widget::container::Style {
                border: iced::Border {
                    color: LINE,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..iced::widget::container::Style::default()
            })
        };
        let segments = subtasks_total.clamp(6, 24);
        let mut seg = row![].spacing(4);
        for i in 0..segments {
            let done = i < subtasks_done;
            seg = seg.push(
                container(text(""))
                    .width(Length::Fill)
                    .height(10)
                    .style(move |_| iced::widget::container::Style {
                        background: Some(Background::Color(if done { OK } else { theme::LINE2 })),
                        border: iced::Border {
                            radius: 2.0.into(),
                            ..iced::Border::default()
                        },
                        ..iced::widget::container::Style::default()
                    }),
            );
        }
        let progress_card = container(
            column![
                row![
                    text("Task progress").size(13).color(theme::HI),
                    text(progress_label.clone()).size(12).color(DIM),
                ]
                .spacing(10),
                seg
            ]
            .spacing(10),
        )
        .padding([12, 14])
        .width(Length::Fill)
        .style(card_style);

        column![
            kpis,
            scrollable(
                column![
                    row![
                        text("Overview").size(26).color(theme::HI),
                        badge("LIVE WORKSPACE", ACCENT),
                        text(ctx.data.root.display().to_string())
                            .size(13)
                            .color(DIM),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                    pulse,
                    row![attention_card, recent_card].spacing(16),
                    progress_card,
                    row![
                        btn_primary("Run a pattern", PanelMsg::OpenLauncher(None)),
                        btn("Manage agents", PanelMsg::Tab(Tab::Agents)),
                        btn_ghost("Open patterns", PanelMsg::Tab(Tab::Patterns)),
                    ]
                    .spacing(10),
                ]
                .spacing(16)
                .padding([22, 24]),
            )
            .height(Length::Fill),
        ]
        .into()
    }

    fn launcher_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let choices: Vec<Choice> = ctx
            .data
            .patterns
            .iter()
            .map(|p| Choice {
                name: p.name.clone(),
                label: format!("{} · {} · {} agent(s)", p.name, p.kind, p.agents_count),
            })
            .collect();
        let selected = self
            .pattern
            .as_ref()
            .and_then(|p| choices.iter().find(|c| c.name == p.name).cloned());
        let workspace = ctx
            .snapshot
            .workspaces
            .iter()
            .find(|w| w.focused)
            .map(|w| w.label.clone())
            .unwrap_or_else(|| "(no space)".into());
        let target = match self.launch_task {
            Some(id) => format!("For task #{id}. The agents join the space “{workspace}”."),
            None => format!("The agents join the space “{workspace}”."),
        };
        column![
            text("RUN PATTERN").size(10).color(DIM),
            pick_list(choices, selected, |c| Message::Panel(
                PanelMsg::PickPattern(c)
            ))
            .placeholder("Pick a pattern")
            .width(Length::Fill),
            text("Objective").size(12).color(DIM),
            text_editor(&self.objective)
                .on_action(|a| Message::Panel(PanelMsg::Edit(a)))
                .height(Length::Fixed(160.0)),
            text(target).size(11).color(DIM),
            row![
                button(container(text("Run pattern").size(13)).center_y(Length::Fill))
                    .padding([0, 16])
                    .height(34)
                    .style(primary_button_style())
                    .on_press(Message::Panel(PanelMsg::Launch)),
                button(container(text("Cancel").size(13)).center_y(Length::Fill))
                    .padding([0, 14])
                    .height(34)
                    .style(theme::outline_button_style(false))
                    .on_press(Message::Panel(PanelMsg::CloseLauncher)),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into()
    }

    fn run_detail<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let Some(r) = self
            .run
            .as_deref()
            .and_then(|id| ctx.data.runs.iter().find(|r| r.run.id == id))
            .or_else(|| ctx.data.runs.first())
        else {
            return container(
                column![
                    text("No pattern runs yet").size(16),
                    text("Launch a pattern to see its agents, health, and communication here.")
                        .size(12)
                        .color(DIM),
                    button(container(text("Run a pattern").size(13)).center_y(Length::Fill))
                        .padding([0, 12])
                        .height(34)
                        .style(primary_button_style())
                        .on_press(Message::Panel(PanelMsg::OpenLauncher(None))),
                ]
                .spacing(8),
            )
            .padding(18)
            .style(card_style)
            .into();
        };
        let run = &r.run;
        let (label, color) = state_badge(run.state);
        let live = matches!(
            run.state,
            PatternRunState::Running | PatternRunState::Stalled | PatternRunState::Degraded
        );
        let mut controls = row![].spacing(8);
        if live {
            controls = controls.push(btn(
                "Pause",
                PanelMsg::Control(run.id.clone(), RunControl::Pause),
            ));
        }
        if run.state == PatternRunState::Paused {
            controls = controls.push(btn(
                "Resume",
                PanelMsg::Control(run.id.clone(), RunControl::Resume),
            ));
        }
        if live || run.state == PatternRunState::Paused {
            let armed = self.armed == Some(Armed::StopRun(run.id.clone()));
            controls = controls.push(self.arm_button(
                Armed::StopRun(run.id.clone()),
                "Stop",
                if armed {
                    "Confirm: stop and close agents"
                } else {
                    "Stop"
                },
            ));
        }
        controls = controls.push(btn("Communication", PanelMsg::ShowMonitor(run.id.clone())));
        controls = controls.push(self.arm_button(
            Armed::DeleteRun(run.id.clone()),
            "Delete run",
            if live || run.state == PatternRunState::Paused {
                "Confirm: stop its agents and delete"
            } else {
                "Confirm: delete this run"
            },
        ));
        let mut col = column![
            row![
                text(format!("RUN: {}", run.pattern))
                    .size(18)
                    .color(theme::HI),
                text(run.id.clone()).size(13).color(DIM),
                badge(label.to_uppercase(), color),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            text(format!(
                "Topology {} · created {} · updated {}",
                run.kind, run.created_at, run.updated_at
            ))
            .size(12)
            .color(DIM),
            controls,
        ]
        .spacing(14);
        if let Some(reason) = run.stalled_reason.as_deref() {
            col = col.push(alert(
                text(format!("STALLED  {reason}"))
                    .size(13)
                    .color(WARN)
                    .into(),
                WARN,
            ));
        }
        if let Some(error) = run.error.as_deref() {
            col = col.push(alert(
                text(format!("ERROR  {error}")).size(13).color(BAD).into(),
                BAD,
            ));
        }
        let mut objective = column![text(if run.task.is_empty() {
            "No objective.".to_owned()
        } else {
            run.task.clone()
        })
        .size(13)]
        .spacing(8);
        if let Some(worktree) = &run.worktree {
            objective = objective.push(text(format!("Worktree: {worktree}")).size(11).color(DIM));
        }
        if !run.topics.is_empty() {
            objective = objective.push(
                text(format!("Topics: {}", run.topics.join(", ")))
                    .size(11)
                    .color(DIM),
            );
        }
        let linked = task_of_run(&run.task).map(|task| {
            btn_link(
                format!("Linked to task #{task}"),
                PanelMsg::SelectTask(task),
            )
        });
        col = col.push(card("OBJECTIVE", linked, objective.into()));
        let mut agents = column![].spacing(6);
        for a in &run.agents {
            let status = agent_status(&a.state);
            agents = agents.push(
                row![
                    agent_square(status),
                    text(a.id.clone()).size(13).color(theme::HI),
                    text(format!("({})", a.role)).size(13).color(DIM),
                    text(format!("[{}]", a.state))
                        .size(13)
                        .color(status_color(status)),
                    text(a.vendor.clone().unwrap_or_default())
                        .size(13)
                        .color(DIM),
                    container(text("")).width(Length::Fill),
                    btn_link("open", PanelMsg::SelectAgent(a.id.clone())),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            );
        }
        col = col.push(card("AGENTS", None, agents.into()));
        container(col).max_width(980).into()
    }

    fn agents_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let working = ctx
            .data
            .agents
            .iter()
            .filter(|a| matches!(a.state.as_str(), "working" | "starting"))
            .count();
        let needs_you = ctx
            .data
            .agents
            .iter()
            .filter(|a| a.state == "blocked" || !a.notice.is_empty())
            .count();
        let finished = ctx
            .data
            .agents
            .iter()
            .filter(|a| matches!(a.state.as_str(), "done" | "exited" | "killed" | "failed"))
            .count();
        let selected = self
            .agent
            .as_deref()
            .and_then(|id| ctx.data.agents.iter().find(|a| a.id == id))
            .or_else(|| ctx.data.agents.first())
            .map(|a| a.id.clone());
        let items = ctx
            .data
            .agents
            .iter()
            .map(|a| {
                let status = agent_status(&a.state);
                let needs_you = a.state == "blocked" || !a.notice.is_empty();
                let body = column![
                    row![
                        agent_square(status),
                        text(a.id.clone()).size(13).color(theme::HI)
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                    text(format!(
                        "{} · {} {} · {}",
                        a.role, a.vendor, a.model, a.state
                    ))
                    .size(11)
                    .color(if needs_you { WARN } else { DIM }),
                ]
                .spacing(1);
                button(body)
                    .width(Length::Fill)
                    .padding([8, 10])
                    .style(item_style(selected.as_deref() == Some(a.id.as_str())))
                    .on_press(Message::Panel(PanelMsg::SelectAgent(a.id.clone())))
                    .into()
            })
            .collect();
        let footer = vec![
            btn_link("+ New agent", PanelMsg::OpenSpawnAgent),
            btn_ghost("Resume idle", PanelMsg::ResumeAgents),
            self.arm_button(
                Armed::CleanRuns,
                "Clean finished",
                "Confirm: delete finished",
            ),
        ];
        let summary = container(
            row![
                agent_metric("TOTAL", ctx.data.agents.len(), FG),
                agent_metric_spark("WORKING", working, ACCENT, ctx.frame),
                agent_metric(
                    "NEEDS YOU",
                    needs_you,
                    if needs_you > 0 { WARN } else { OK }
                ),
                agent_metric("FINISHED", finished, DIM),
            ]
            .spacing(1),
        )
        .style(kpi_strip_style);
        let detail: Element<'a, Message> = if self.spawning_agent {
            self.spawn_agent_view(ctx)
        } else {
            match ctx
                .data
                .agents
                .iter()
                .find(|a| Some(a.id.as_str()) == selected.as_deref())
            {
                None => text("No agents. Launch one with \"+ New agent\".")
                    .color(DIM)
                    .into(),
                Some(a) => self.agent_detail(ctx, a),
            }
        };
        column![
            summary,
            row![self.list(items, footer), self.detail(detail)].height(Length::Fill),
        ]
        .spacing(8)
        .into()
    }

    /// Form for launching a single agent with a Shepherd role.
    /// The detail of one agent (`V.agents`): header, actions, live output, approval notice and context cards.
    fn agent_detail<'a>(
        &'a self,
        ctx: &Ctx<'a>,
        a: &'a shepherd_app_core::shepherd_core::Agent,
    ) -> Element<'a, Message> {
        let status = agent_status(&a.state);
        let linked_run = ctx
            .data
            .runs
            .iter()
            .find(|run| run.run.agents.iter().any(|ra| ra.id == a.id));
        let mut sub = format!("{} · {} {}", a.role, a.vendor, a.model);
        if let Some(run) = linked_run {
            sub.push_str(&format!(" · run {}", run.run.pattern));
        }
        let action = |label: &'static str, msg: Message, primary: bool| -> Element<'a, Message> {
            button(container(text(label).size(13)).center_y(Length::Fill))
                .padding([0, 12])
                .height(34)
                .style(if primary {
                    Box::new(primary_button_style())
                        as Box<dyn Fn(&iced::Theme, button::Status) -> button::Style>
                } else {
                    Box::new(theme::outline_button_style(false))
                })
                .on_press(msg)
                .into()
        };
        let mut actions = row![].spacing(8);
        if !a.pane.is_empty() {
            actions = actions.push(action("Terminal", Message::GoToPane(a.pane.clone()), true));
        }
        actions = actions.push(action(
            "Focus agent",
            Message::Act(Action::FocusAgent(a.id.clone())),
            false,
        ));
        if !a.workspace.is_empty() {
            actions = actions.push(action(
                "Workspace",
                Message::Act(Action::FocusSpace(a.workspace.clone())),
                false,
            ));
        }
        if !a.worktree.is_empty() || !a.branch.is_empty() {
            actions = actions.push(btn("Git", PanelMsg::Tab(Tab::Git)));
        }
        if !["done", "exited", "killed", "failed"]
            .iter()
            .any(|s| a.state == *s)
        {
            actions = actions.push(
                button(container(text("Close agent").size(13)).center_y(Length::Fill))
                    .padding([0, 12])
                    .height(34)
                    .style(danger_button_style())
                    .on_press(Message::Panel(PanelMsg::KillAgent(a.id.clone()))),
            );
        }
        let mut col = column![
            row![
                text(a.id.clone()).size(18).color(theme::HI),
                text(sub).size(12).color(DIM),
                badge(a.state.to_uppercase(), status_color(status)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            actions,
        ]
        .spacing(14);

        // Live output: the latest messages of this agent (the terminal itself lives in its pane).
        let mut output = column![].spacing(6);
        let recent: Vec<_> = self.feed.iter().rev().take(6).map(feed_line).collect();
        if recent.is_empty() {
            output = output.push(
                text("No recent communication for this agent.")
                    .size(12)
                    .color(DIM),
            );
        }
        for (head, body) in recent {
            output = output
                .push(text(head).size(11).color(ACCENT))
                .push(text(body).size(12.5));
        }
        let pane =
            (!a.pane.is_empty()).then(|| text(format!("pane {}", a.pane)).size(11).color(DIM));
        col = col.push(card("LIVE OUTPUT", pane.map(Element::from), output.into()));

        if !a.notice.is_empty() {
            let body = column![text(a.notice.clone()).size(13).color(theme::HI)].spacing(8);
            col = col.push(
                container(column![
                    container(theme::label("APPROVAL REQUIRED").color(WARN)).padding([10, 14]),
                    container(body).padding(pad(4.0, 14.0, 12.0, 14.0)),
                ])
                .width(Length::Fill)
                .style(theme::alert_style(WARN)),
            );
        }
        if !a.task_text.is_empty() {
            col = col.push(card(
                "TASK",
                None,
                text(a.task_text.clone()).size(13).into(),
            ));
        }
        if !a.result.is_empty() {
            col = col.push(card("RESULT", None, text(a.result.clone()).size(13).into()));
        }
        if !a.worktree.is_empty() || !a.branch.is_empty() {
            col = col.push(card(
                "SOURCE CONTROL",
                None,
                column![
                    text(if a.branch.is_empty() {
                        "detached HEAD".to_owned()
                    } else {
                        format!("branch {}", a.branch)
                    })
                    .size(13)
                    .color(ACCENT),
                    text(if a.worktree.is_empty() {
                        "No dedicated worktree".to_owned()
                    } else {
                        a.worktree.clone()
                    })
                    .size(11)
                    .color(DIM),
                ]
                .spacing(4)
                .into(),
            ));
        }
        if let Some(run) = linked_run {
            let (run_state, run_color) = state_badge(run.run.state);
            col = col.push(card(
                "RUN CONTEXT",
                Some(btn_link(
                    "Open communication",
                    PanelMsg::ShowMonitor(run.run.id.clone()),
                )),
                column![
                    text(format!("{} · {}", run.run.pattern, run_state))
                        .size(13)
                        .color(run_color),
                    text(if run.run.task.is_empty() {
                        "No objective.".to_owned()
                    } else {
                        run.run.task.clone()
                    })
                    .size(12),
                ]
                .spacing(4)
                .into(),
            ));
        }
        if !ctx.data.tasks.is_empty() {
            let task_choices: Vec<Choice> = ctx
                .data
                .tasks
                .iter()
                .map(|t| Choice {
                    name: t.status.id.to_string(),
                    label: format!("#{} {}", t.status.id, t.status.title),
                })
                .collect();
            let current = task_choices.iter().find(|c| c.name == a.task_id).cloned();
            col = col.push(card(
                "ASSIGN TO TASK",
                None,
                pick_list(task_choices, current, |c| {
                    Message::Panel(PanelMsg::AssignAgentTask(
                        a.id.clone(),
                        Some(c.name.parse().unwrap_or(0)),
                    ))
                })
                .placeholder("(none)")
                .width(Length::Fill)
                .into(),
            ));
        }
        container(col).max_width(980).into()
    }

    fn spawn_agent_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let role_choices: Vec<Choice> = ctx
            .data
            .models
            .roles
            .iter()
            .map(|(name, role)| Choice {
                name: name.clone(),
                label: format!(
                    "{} · {}{}",
                    name,
                    role.vendor,
                    if !role.model.is_empty() {
                        format!(" / {}", role.model)
                    } else {
                        String::new()
                    }
                ),
            })
            .collect();
        let selected_role = self
            .spawn_role
            .as_ref()
            .and_then(|r| role_choices.iter().find(|c| c.name == r.name).cloned());
        let workspace_choices: Vec<Choice> = ctx
            .snapshot
            .workspaces
            .iter()
            .map(|w| Choice {
                name: w.workspace_id.clone(),
                label: w.label.clone(),
            })
            .collect();
        let selected_workspace = self
            .spawn_workspace
            .as_ref()
            .and_then(|w| workspace_choices.iter().find(|c| c.name == w.name).cloned());
        let vendor_choices: Vec<Choice> = ctx
            .data
            .models
            .vendors
            .iter()
            .map(|(name, v)| Choice {
                name: name.clone(),
                label: format!(
                    "{}{}",
                    name,
                    if !v.command.is_empty() {
                        " ✓"
                    } else {
                        " — not installed"
                    }
                ),
            })
            .collect();
        let selected_vendor = self
            .spawn_vendor
            .as_ref()
            .and_then(|v| vendor_choices.iter().find(|c| c.name == v.name).cloned());
        let workspace_label = self
            .spawn_workspace
            .as_ref()
            .map(|w| w.label.clone())
            .unwrap_or_else(|| "(no space)".into());
        column![
            text("LAUNCH AGENT").size(10).color(DIM),
            text("Role").size(12).color(DIM),
            pick_list(role_choices, selected_role, |c| Message::Panel(
                PanelMsg::SpawnAgentRole(c)
            ))
            .placeholder("Pick a role")
            .width(Length::Fill),
            text("Workspace").size(12).color(DIM),
            pick_list(workspace_choices, selected_workspace, |c| Message::Panel(
                PanelMsg::SpawnAgentWorkspace(c)
            ))
            .placeholder("Pick a workspace")
            .width(Length::Fill),
            text("Vendor (optional, from role)").size(12).color(DIM),
            pick_list(vendor_choices, selected_vendor, |c| Message::Panel(
                PanelMsg::SpawnAgentVendor(c)
            ))
            .placeholder("Pick a vendor")
            .width(Length::Fill),
            text("Model (optional)").size(12).color(DIM),
            text_input("Model name", &self.spawn_model)
                .on_input(|v| Message::Panel(PanelMsg::SpawnAgentModel(v)))
                .width(Length::Fill),
            text("Task").size(12).color(DIM),
            text_input("Agent task description", &self.spawn_task_text)
                .on_input(|v| Message::Panel(PanelMsg::SpawnAgentTask(v)))
                .width(Length::Fill),
            checkbox(self.spawn_worktree)
                .label("in its own worktree")
                .on_toggle(|on| Message::Panel(PanelMsg::SpawnAgentWorktree(on)))
                .text_size(12),
            text(format!("The agent joins the space \"{workspace_label}\"."))
                .size(11)
                .color(DIM),
            row![
                button(container(text("Launch").size(13)).center_y(Length::Fill))
                    .padding([0, 16])
                    .height(34)
                    .style(primary_button_style())
                    .on_press(Message::Panel(PanelMsg::SpawnAgent)),
                button(container(text("Cancel").size(13)).center_y(Length::Fill))
                    .padding([0, 14])
                    .height(34)
                    .style(theme::outline_button_style(false))
                    .on_press(Message::Panel(PanelMsg::CloseSpawnAgent)),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into()
    }

    fn tasks_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let is_done = |state: &str| matches!(state, "done" | "completed" | "archived");
        let open = ctx
            .data
            .tasks
            .iter()
            .filter(|t| !is_done(&t.status.state))
            .count();
        let in_run = ctx
            .data
            .tasks
            .iter()
            .filter(|t| {
                ctx.data
                    .runs
                    .iter()
                    .any(|r| task_of_run(&r.run.task) == Some(t.status.id))
            })
            .count();
        let (sub_done, sub_total) = ctx.data.tasks.iter().fold((0, 0), |(d, n), t| {
            (
                d + t
                    .status
                    .subtasks
                    .iter()
                    .filter(|s| matches!(s.state.as_str(), "done" | "completed"))
                    .count(),
                n + t.status.subtasks.len(),
            )
        });
        let summary = container(
            row![
                kpi_cell("OPEN", open.to_string(), WARN, None),
                kpi_cell("IN RUN", in_run.to_string(), ACCENT, None),
                kpi_cell(
                    "SUBTASKS DONE",
                    format!("{sub_done}/{sub_total}"),
                    theme::HI,
                    None
                ),
                kpi_cell(
                    "DONE",
                    ctx.data
                        .tasks
                        .iter()
                        .filter(|t| is_done(&t.status.state))
                        .count()
                        .to_string(),
                    OK,
                    None
                ),
            ]
            .spacing(1),
        )
        .style(kpi_strip_style);
        let selected = self
            .task
            .and_then(|id| ctx.data.task(id))
            .or_else(|| ctx.data.tasks.first())
            .map(|t| t.status.id);
        let items = ctx
            .data
            .tasks
            .iter()
            .map(|t| {
                let in_run = ctx
                    .data
                    .runs
                    .iter()
                    .find(|r| task_of_run(&r.run.task) == Some(t.status.id));
                let body = column![
                    text(format!("#{} {}", t.status.id, t.status.title))
                        .size(13)
                        .color(theme::HI),
                    text(match in_run {
                        Some(r) => format!("{} · run {}", t.status.state, r.run.pattern),
                        None => t.status.state.clone(),
                    })
                    .size(11)
                    .color(task_state_color(&t.status.state)),
                ]
                .spacing(1);
                button(body)
                    .width(Length::Fill)
                    .padding([8, 10])
                    .style(item_style(selected == Some(t.status.id)))
                    .on_press(Message::Panel(PanelMsg::SelectTask(t.status.id)))
                    .into()
            })
            .collect();
        let footer = vec![
            btn_link("+ Task", PanelMsg::TaskNew),
            self.arm_button(Armed::ArchiveDone, "Archive done", "Confirm: archive done"),
        ];
        let detail: Element<'a, Message> = match ctx
            .data
            .tasks
            .iter()
            .find(|t| Some(t.status.id) == selected)
        {
            None => text(format!("No tasks in {}.", ctx.data.root.display()))
                .color(DIM)
                .into(),
            Some(t) => {
                let runs: Vec<_> = ctx
                    .data
                    .runs
                    .iter()
                    .filter(|r| task_of_run(&r.run.task) == Some(t.status.id))
                    .collect();
                let state_color = task_state_color(&t.status.state);
                let mut col = column![
                    row![
                        text(format!("Task #{} · {}", t.status.id, t.status.title))
                            .size(18)
                            .color(theme::HI),
                        badge(t.status.state.to_uppercase(), state_color),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
                    text(format!(
                        "branch {}",
                        if t.status.branch.is_empty() {
                            "none"
                        } else {
                            &t.status.branch
                        }
                    ))
                    .size(12)
                    .color(DIM),
                ]
                .spacing(14);
                if !t.description.is_empty() {
                    col = col.push(text(t.description.clone()).size(13));
                }
                if !t.criteria.is_empty() {
                    let mut rows = column![].spacing(8);
                    for c in &t.criteria {
                        let text_of = c
                            .trim_start_matches("- [ ] ")
                            .trim_start_matches("- [x] ")
                            .trim_start_matches("- ")
                            .to_owned();
                        rows = rows.push(
                            row![
                                container(text("")).width(18).height(18).style(|_| {
                                    iced::widget::container::Style {
                                        border: iced::Border {
                                            color: theme::LINE2,
                                            width: 1.0,
                                            radius: 3.0.into(),
                                        },
                                        ..iced::widget::container::Style::default()
                                    }
                                }),
                                text(text_of).size(13).width(Length::Fill),
                            ]
                            .spacing(10),
                        );
                    }
                    col = col.push(card("ACCEPTANCE CRITERIA", None, rows.into()));
                }
                if !t.status.subtasks.is_empty() {
                    let mut rows = column![].spacing(6);
                    for subtask in &t.status.subtasks {
                        let state = subtask.state.as_str();
                        let note = subtask
                            .note
                            .as_deref()
                            .filter(|note| !note.is_empty())
                            .map(|note| format!(" · {note}"))
                            .unwrap_or_default();
                        rows = rows.push(
                            row![
                                agent_square(agent_status(state)),
                                text(subtask.title.clone()).size(13).color(theme::HI),
                                text(format!("{state}{note}")).size(11).color(DIM),
                            ]
                            .spacing(10)
                            .align_y(iced::Alignment::Center),
                        );
                    }
                    col = col.push(card("SUBTASKS", None, rows.into()));
                }
                let mut actions = row![].spacing(10);
                for r in runs {
                    actions = actions.push(btn(
                        format!("Open run {}", r.run.pattern),
                        PanelMsg::SelectRun(r.run.id.clone()),
                    ));
                }
                actions = actions
                    .push(btn_primary(
                        "Run with an agent",
                        PanelMsg::RunTaskWithAgent(t.status.id),
                    ))
                    .push(btn(
                        "Run with a pattern",
                        PanelMsg::OpenLauncher(Some(t.status.id)),
                    ))
                    .push(btn("Edit", PanelMsg::TaskEditOpen(t.status.id)))
                    .push(self.arm_button(
                        Armed::DeleteTask(t.status.id),
                        "Delete",
                        "Confirm: delete this task for good",
                    ));
                col.push(actions).max_width(900).into()
            }
        };
        let detail = match &self.task_form {
            Some(form) => self.task_form_view(form),
            None => detail,
        };
        column![
            summary,
            row![self.list(items, footer), self.detail(detail)].height(Length::Fill),
        ]
        .into()
    }

    /// The form that creates a task or edits one: title, description, criteria (a line each) and, when editing,
    /// the spec.
    fn task_form_view<'a>(&'a self, form: &'a TaskForm) -> Element<'a, Message> {
        let editor = |content: &'a text_editor::Content,
                      action: fn(text_editor::Action) -> PanelMsg,
                      height: f32| {
            text_editor(content)
                .on_action(move |a| Message::Panel(action(a)))
                .height(Length::Fixed(height))
                .padding(10)
                .size(13)
                .style(theme::editor_style)
        };
        let mut col = column![
            row![
                text(match form.editing {
                    Some(id) => format!("Edit task #{id}"),
                    None => "New task".to_owned(),
                })
                .size(18)
                .color(theme::HI),
                container(text("")).width(Length::Fill),
                btn_primary("Save", PanelMsg::TaskSave),
                btn("Cancel", PanelMsg::TaskCancel),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            column![
                theme::label("TITLE"),
                text_input("What has to be done", &form.title)
                    .on_input(|t| Message::Panel(PanelMsg::TaskTitle(t)))
                    .on_submit(Message::Panel(PanelMsg::TaskSave))
                    .style(theme::input_style)
                    .padding([8, 10])
                    .size(13),
            ]
            .spacing(6),
            column![
                theme::label("DESCRIPTION"),
                editor(&form.description, PanelMsg::TaskDescription, 110.0),
            ]
            .spacing(6),
            column![
                theme::label("ACCEPTANCE CRITERIA · ONE PER LINE"),
                editor(&form.criteria, PanelMsg::TaskCriteria, 130.0),
            ]
            .spacing(6),
        ]
        .spacing(14);
        if form.editing.is_some() {
            col = col.push(
                column![
                    theme::label("SPEC"),
                    editor(&form.spec, PanelMsg::TaskSpec, 160.0),
                ]
                .spacing(6),
            );
        }
        container(col).max_width(900).into()
    }

    fn patterns_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let total_agents: usize = ctx.data.patterns.iter().map(|p| p.agents_count).sum();
        let total_topics: usize = ctx.data.patterns.iter().map(|p| p.topics_count).sum();
        let summary = container(
            row![
                agent_metric("PATTERNS", ctx.data.patterns.len(), FG),
                agent_metric("AGENTS", total_agents, ACCENT),
                agent_metric("TOPICS", total_topics, OK),
            ]
            .spacing(1),
        )
        .style(kpi_strip_style);
        let items = ctx
            .data
            .patterns
            .iter()
            .map(|p| {
                let body = column![
                    text(p.name.clone()).size(13),
                    text(format!(
                        "{:?} · {} agents · {} topics",
                        p.kind, p.agents_count, p.topics_count
                    ))
                    .size(11)
                    .color(DIM)
                ];
                button(body)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(item_style(
                        self.pattern_sel.as_deref() == Some(p.name.as_str()),
                    ))
                    .on_press(Message::Panel(PanelMsg::SelectPattern(p.name.clone())))
                    .into()
            })
            .collect();
        let detail: Element<'a, Message> = match &self.edit {
            None => {
                let message = if ctx.data.patterns.is_empty() {
                    "No patterns in this project. Create one from the configuration or add a pattern file."
                } else if self.pattern_sel.is_some() {
                    "Loading pattern…"
                } else {
                    "Pick a pattern to inspect and edit its graph."
                };
                container(
                    column![
                        text(if ctx.data.patterns.is_empty() {
                            "No patterns yet"
                        } else {
                            "Select a pattern"
                        })
                        .size(16),
                        text(message).size(12).color(DIM),
                    ]
                    .spacing(8),
                )
                .padding(18)
                .style(card_style)
                .into()
            }
            Some(edit) => column![edit.view(ctx.frame),].spacing(10).into(),
        };
        column![
            summary,
            row![
                self.list(items, vec![]),
                container(detail)
                    .padding([20, 24])
                    .width(Length::Fill)
                    .height(Length::Fill)
            ]
            .height(Length::Fill),
        ]
        .spacing(8)
        .into()
    }

    fn monitor_view<'a>(&'a self, ctx: &Ctx<'a>) -> Element<'a, Message> {
        let mut items: Vec<Element<'a, Message>> = vec![button(text("All agents").size(13))
            .width(Length::Fill)
            .padding([4, 8])
            .style(item_style(self.monitor.is_none()))
            .on_press(Message::Panel(PanelMsg::ShowMonitor(String::new())))
            .into()];
        for r in &ctx.data.runs {
            items.push(
                button(
                    text(format!(
                        "{} · {}",
                        r.run.pattern,
                        r.run.id.trim_start_matches("run-")
                    ))
                    .size(13),
                )
                .width(Length::Fill)
                .padding([4, 8])
                .style(item_style(
                    self.monitor.as_deref() == Some(r.run.id.as_str()),
                ))
                .on_press(Message::Panel(PanelMsg::ShowMonitor(r.run.id.clone())))
                .into(),
            );
        }
        let key = if self.armed == Some(Armed::CleanBusForce) {
            Armed::CleanBusForce
        } else {
            Armed::CleanBus
        };
        let force = key == Armed::CleanBusForce;
        let footer = vec![self.arm_button(
            key,
            "Clean communication",
            if force {
                "Live agents: force clean"
            } else {
                "Confirm: delete messages"
            },
        )];
        let summary = container(
            row![
                agent_metric("RUNS", ctx.data.runs.len(), FG),
                agent_metric_spark("MESSAGES", self.feed.len(), ACCENT, ctx.frame),
                agent_metric(
                    "SCOPE",
                    if self.monitor.is_some() {
                        1
                    } else {
                        ctx.data.agents.len()
                    },
                    OK,
                ),
            ]
            .spacing(1),
        )
        .style(kpi_strip_style);
        let mut feed = column![row![
            theme::label("COMMUNICATION"),
            text(format!("{} messages", self.feed.len()))
                .size(12)
                .color(ACCENT),
        ]
        .spacing(8)]
        .spacing(6);
        if self.feed.is_empty() {
            feed = feed.push(
                container(
                    column![
                        text("No communication yet").size(15),
                        text("Messages will appear here as agents publish to the selected scope.")
                            .size(12)
                            .color(DIM),
                    ]
                    .spacing(8),
                )
                .padding(16)
                .style(card_style),
            );
        }
        for m in &self.feed {
            feed = feed.push(message_row(m, false));
        }
        column![
            summary,
            row![self.list(items, footer), self.detail(feed.into())].height(Length::Fill),
        ]
        .spacing(8)
        .into()
    }
}

/// `.list`: 300 px column with a 1 px line on its right, items on top and the footer buttons below.
pub fn list_box<'a>(
    items: Vec<Element<'a, Message>>,
    footer: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let line = || {
        container(text(""))
            .width(Length::Fill)
            .height(1)
            .style(|_| container::Style {
                background: Some(Background::Color(LINE)),
                ..container::Style::default()
            })
    };
    let list = scrollable(column(items).spacing(2).padding(8)).height(Length::Fill);
    row![
        container(column![
            list,
            line(),
            container(row(footer).spacing(4).wrap()).padding(8)
        ])
        .width(300)
        .height(Length::Fill),
        container(text(""))
            .width(1)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(LINE)),
                ..container::Style::default()
            }),
    ]
    .height(Length::Fill)
    .into()
}

/// `.detail`: the rest of the width, padded, no card.
pub fn detail_box<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(scrollable(
        container(content).padding([20, 24]).width(Length::Fill),
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_app_core::data::TaskRow;
    use shepherd_app_core::shepherd_core::TaskStatus;

    fn task(id: u32) -> TaskRow {
        TaskRow {
            status: TaskStatus {
                id,
                slug: "t".into(),
                title: "Login".into(),
                state: "new".into(),
                issue: None,
                branch: String::new(),
                pr: 0,
                subtasks: vec![],
                waiting: String::new(),
                note: String::new(),
                agents: vec![],
                created: String::new(),
                updated: String::new(),
            },
            description: "Para entrar".into(),
            criteria: vec!["- [ ] funciona".into()],
            issue_body: "Para entrar".into(),
            spec: String::new(),
        }
    }

    #[test]
    fn the_launcher_for_a_task_starts_from_its_text_and_stop_needs_two_clicks() {
        let data = Data {
            tasks: vec![task(3)],
            ..Data::default()
        };
        let snapshot = Snapshot::default();
        let ctx = Ctx {
            api: PathBuf::from("/nowhere"),
            data: &data,
            snapshot: &snapshot,
            frame: 0,
        };
        let mut panel = Panel::default();
        let _ = panel.update(PanelMsg::OpenLauncher(Some(3)), &ctx);
        assert!(panel.launching);
        assert_eq!(panel.tab, Tab::Runs);
        assert!(panel
            .objective
            .text()
            .starts_with("Task #3: Login\n\nPara entrar"));

        // the first click on Stop only arms it; anything else disarms it
        let _ = panel.update(PanelMsg::Control("run-1".into(), RunControl::Stop), &ctx);
        assert_eq!(panel.armed, Some(Armed::StopRun("run-1".into())));
        let _ = panel.update(PanelMsg::Tab(Tab::Agents), &ctx);
        assert_eq!(panel.armed, None);
    }
}
