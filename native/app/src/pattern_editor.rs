//! The pattern editor: the graph with a toolbar and an inspector. The rules (what connects to what, renaming,
//! saving) live in `shepherd_app_core::pattern_edit`; this file only holds the selection and draws.

use crate::theme::{primary_button_style, surface_style, ACCENT, DIM};
use crate::Message;
use iced::widget::{
    button, canvas::Canvas, checkbox, column, container, row, scrollable, text, text_input,
};
use iced::{Element, Length};
use shepherd_app_core::pattern_edit::{AgentField, Editor, Sel};
use shepherd_app_core::pattern_graph::{relayout_keep, Graph, PatternView, NODE_H, NODE_W};
use shepherd_app_core::shepherd_core::config::{self, Config};
use shepherd_app_core::shepherd_core::patterns::{self, Severity};
use std::path::Path;

#[derive(Debug, Clone)]
pub enum EditMsg {
    Press(String),
    Move(String, f32, f32),
    AddAgent,
    AddTopic,
    ToggleConnect,
    DeleteSelected,
    Duplicate,
    Save,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    Field(Field, String),
    Start(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    PatternName,
    Description,
    Agent(AgentField),
    TopicName,
    Partitions,
}

pub struct PatternEditor {
    pub editor: Editor,
    pub graph: Graph,
    pub selected: Option<Sel>,
    connecting: Option<Sel>,
    cfg: Option<Config>,
    pub problems: Vec<(bool, String, String)>,
    /// What the inspector's boxes show while typing (a half-typed number is not a valid value yet).
    pub fields: Vec<(Field, String)>,
    pub path: Option<String>,
    pub zoom: f32,
    delete_armed: bool,
}

impl PatternEditor {
    pub fn from_view(view: PatternView) -> Self {
        let mut me = Self {
            editor: Editor::open(view.pattern, view.source),
            graph: view.graph,
            // SHEPHERD_NATIVE_SELECT=agent:<id> starts with that node selected (for pictures)
            selected: std::env::var("SHEPHERD_NATIVE_SELECT")
                .ok()
                .and_then(|k| Sel::from_key(&k)),
            connecting: None,
            cfg: None,
            problems: view.problems,
            fields: Vec::new(),
            path: view.path,
            zoom: 1.0,
            delete_armed: false,
        };
        me.refresh_fields();
        me
    }

    fn refresh_fields(&mut self) {
        let p = &self.editor.pattern;
        self.fields = vec![
            (Field::PatternName, p.name.clone()),
            (Field::Description, p.description.clone()),
        ];
        match &self.selected {
            Some(Sel::Agent(id)) => {
                if let Some(a) = p.agents.iter().find(|a| &a.id == id) {
                    self.fields.extend([
                        (Field::Agent(AgentField::Id), a.id.clone()),
                        (Field::Agent(AgentField::Role), a.role.clone()),
                        (
                            Field::Agent(AgentField::Vendor),
                            a.vendor.clone().unwrap_or_default(),
                        ),
                        (
                            Field::Agent(AgentField::Model),
                            a.model.clone().unwrap_or_default(),
                        ),
                        (Field::Agent(AgentField::Count), a.count.to_string()),
                        (
                            Field::Agent(AgentField::Level),
                            a.level.map(|l| l.to_string()).unwrap_or_default(),
                        ),
                        (
                            Field::Agent(AgentField::Group),
                            a.group.clone().unwrap_or_default(),
                        ),
                    ]);
                }
            }
            Some(Sel::Topic(name)) => {
                if let Some(t) = p.topics.iter().find(|t| &t.name == name) {
                    self.fields.extend([
                        (Field::TopicName, t.name.clone()),
                        (Field::Partitions, t.partitions.to_string()),
                    ]);
                }
            }
            None => {}
        }
    }

    fn field(&self, field: Field) -> &str {
        self.fields
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }

    /// Re-validates after a change; the config is read once.
    fn revalidate(&mut self, root: &Path) {
        if self.cfg.is_none() {
            self.cfg = config::load_config(root).ok();
        }
        if let Some(cfg) = &self.cfg {
            self.problems = patterns::validate(&self.editor.pattern, cfg)
                .into_iter()
                .map(|p| (matches!(p.severity, Severity::Error), p.path, p.message))
                .collect();
        }
    }

    fn changed(&mut self, root: &Path) {
        self.graph = relayout_keep(&self.graph, &self.editor.pattern);
        self.revalidate(root);
    }

    /// Applies one edit; the result is a note for the status line.
    pub fn update(&mut self, msg: EditMsg, root: &Path) -> Option<String> {
        if !matches!(msg, EditMsg::DeleteSelected) {
            self.delete_armed = false;
        }
        match msg {
            EditMsg::Press(key) => {
                let target = Sel::from_key(&key);
                if let (Some(from), Some(to)) = (self.connecting.take(), target.clone()) {
                    return match self.editor.connect(&from, &to) {
                        Ok(()) => {
                            self.changed(root);
                            Some(format!("Connected {} → {}", from.key(), to.key()))
                        }
                        Err(e) => Some(e),
                    };
                }
                self.selected = target;
                self.refresh_fields();
            }
            EditMsg::Move(key, x, y) => {
                if let Some(node) = self.graph.nodes.iter_mut().find(|n| n.key == key) {
                    node.x = x.max(0.0);
                    node.y = y.max(0.0);
                }
                let (w, h) = self.graph.nodes.iter().fold((0.0f32, 0.0f32), |(w, h), n| {
                    (w.max(n.x + NODE_W), h.max(n.y + NODE_H))
                });
                self.graph.width = self.graph.width.max(w + 20.0);
                self.graph.height = self.graph.height.max(h + 20.0);
            }
            EditMsg::AddAgent => {
                self.selected = Some(self.editor.add_agent());
                self.changed(root);
                self.refresh_fields();
            }
            EditMsg::AddTopic => {
                self.selected = Some(self.editor.add_topic());
                self.changed(root);
                self.refresh_fields();
            }
            EditMsg::ToggleConnect => {
                self.connecting = match (self.connecting.take(), self.selected.clone()) {
                    (None, Some(from)) => Some(from),
                    _ => None,
                };
                return Some(if self.connecting.is_some() {
                    "Click the node to connect to (agent → topic publishes, topic → agent consumes).".into()
                } else {
                    "Select a node first, then Connect.".into()
                });
            }
            EditMsg::DeleteSelected => {
                if self.selected.is_none() {
                    return Some("Select a node before deleting it.".into());
                }
                if !self.delete_armed {
                    self.delete_armed = true;
                    return Some("Press Delete again to remove the selected node.".into());
                }
                if let Some(sel) = self.selected.take() {
                    self.editor.delete(&sel);
                    self.changed(root);
                    self.refresh_fields();
                }
                self.delete_armed = false;
            }
            EditMsg::Duplicate => {
                let name = format!("{}-copy", self.editor.pattern.name);
                self.editor = self.editor.duplicate(&name);
                self.path = None;
                self.refresh_fields();
                return Some(format!(
                    "Duplicated as {name}: rename it if you like, then Save."
                ));
            }
            EditMsg::ZoomIn => self.zoom = (self.zoom + 0.1).min(2.0),
            EditMsg::ZoomOut => self.zoom = (self.zoom - 0.1).max(0.5),
            EditMsg::ResetZoom => self.zoom = 1.0,
            EditMsg::Save => {
                return Some(match self.editor.save(root) {
                    Ok(path) => {
                        self.path = Some(path.display().to_string());
                        format!("Saved {}", path.display())
                    }
                    Err(e) => e,
                });
            }
            EditMsg::Start(on) => {
                if let Some(Sel::Agent(id)) = self.selected.clone() {
                    self.editor.set_start(&id, on);
                    self.changed(root);
                }
            }
            EditMsg::Field(field, value) => {
                if let Some(slot) = self.fields.iter_mut().find(|(f, _)| *f == field) {
                    slot.1 = value.clone();
                }
                let result = match (field, self.selected.clone()) {
                    (Field::PatternName, _) if self.editor.is_new => {
                        self.editor.pattern.name = value.trim().to_owned();
                        self.editor.dirty = true;
                        Ok(())
                    }
                    (Field::PatternName, _) => {
                        Err("Only a new pattern can be renamed: duplicate it first.".to_owned())
                    }
                    (Field::Description, _) => {
                        self.editor.pattern.description = value;
                        self.editor.dirty = true;
                        Ok(())
                    }
                    (Field::Agent(f), Some(Sel::Agent(id))) => {
                        let r = self.editor.set_agent_field(&id, f, &value);
                        if f == AgentField::Id && r.is_ok() {
                            self.selected = Some(Sel::Agent(value.trim().to_owned()));
                        }
                        r
                    }
                    (Field::TopicName, Some(sel @ Sel::Topic(_))) => self
                        .editor
                        .rename(&sel, &value)
                        .map(|new| self.selected = Some(new)),
                    (Field::Partitions, Some(Sel::Topic(name))) => {
                        self.editor.set_partitions(&name, &value)
                    }
                    _ => Ok(()),
                };
                self.changed(root);
                return result.err();
            }
        }
        None
    }

    fn labelled<'a>(&'a self, label: &'static str, field: Field) -> Element<'a, Message> {
        row![
            container(text(label).size(12).color(DIM)).width(90),
            text_input("", self.field(field))
                .style(crate::theme::input_style)
                .on_input(move |v| Message::Panel(crate::panel::PanelMsg::EditPattern(
                    EditMsg::Field(field, v)
                )))
                .size(13)
                .padding([8, 10])
                .width(Length::Fill),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into()
    }

    pub fn view<'a>(&'a self, frame: u32) -> Element<'a, Message> {
        let edit = |m: EditMsg| Message::Panel(crate::panel::PanelMsg::EditPattern(m));
        let tool = |label: &'static str, msg: EditMsg, on: bool| {
            button(container(text(label).size(13)).center_y(Length::Fill))
                .padding([0, 12])
                .height(34)
                .style(crate::theme::outline_button_style(!on))
                .on_press(edit(msg))
        };
        let read_only = self.editor.read_only();
        let mut toolbar = row![
            tool("+ Agent", EditMsg::AddAgent, false),
            tool("+ Topic", EditMsg::AddTopic, false),
            tool("Connect", EditMsg::ToggleConnect, self.connecting.is_some()),
            tool(
                if self.delete_armed {
                    "Confirm delete"
                } else {
                    "Delete"
                },
                EditMsg::DeleteSelected,
                self.delete_armed,
            ),
            tool("Duplicate", EditMsg::Duplicate, false)
        ]
        .spacing(6);
        toolbar = toolbar
            .push(tool("−", EditMsg::ZoomOut, false))
            .push(
                text(format!("{:.0}%", self.zoom * 100.0))
                    .size(12)
                    .color(DIM),
            )
            .push(tool("+", EditMsg::ZoomIn, false))
            .push(tool("Reset", EditMsg::ResetZoom, false));
        let state = if read_only {
            "built-in: read-only, duplicate it to change it"
        } else if self.editor.dirty {
            "● unsaved changes"
        } else {
            "saved"
        };
        let valid: Element<'a, Message> = if self.problems.is_empty() {
            text("✓ valid").size(12).color(crate::theme::OK).into()
        } else {
            text("").into()
        };
        let mut col = column![
            row![
                text(format!("PATTERN {}", self.editor.pattern.name))
                    .size(18)
                    .color(crate::theme::HI),
                valid,
                container(text("")).width(Length::Fill),
                button(container(text("▶ Run this pattern").size(13)).center_y(Length::Fill))
                    .padding([0, 12])
                    .height(34)
                    .style(primary_button_style())
                    .on_press(Message::Panel(crate::panel::PanelMsg::PickAndLaunch(
                        self.editor.pattern.name.clone()
                    ))),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            text(format!(
                "{:?} · {:?} · {state}",
                self.editor.pattern.kind, self.editor.source
            ))
            .size(12)
            .color(DIM),
            toolbar.align_y(iced::Alignment::Center),
        ]
        .spacing(10);
        if let Some(path) = &self.path {
            col = col.push(text(path.clone()).size(11).color(DIM));
        }
        for (is_error, place, message) in &self.problems {
            col = col.push(
                text(format!(
                    "{} {place}: {message}",
                    if *is_error { "✖" } else { "▲" }
                ))
                .size(12)
                .color(if *is_error {
                    crate::theme::BAD
                } else {
                    crate::theme::WARN
                }),
            );
        }
        // inspector
        let mini_zoom = (280.0 / self.graph.width.max(1.0))
            .min(92.0 / self.graph.height.max(1.0))
            .clamp(0.08, 0.32);
        let mini_map = container(
            Canvas::new(crate::graph_view::GraphView {
                graph: &self.graph,
                selected: self.selected.as_ref().map(Sel::key),
                connecting: self.connecting.as_ref().map(Sel::key),
                zoom: mini_zoom,
                frame,
                mini: true,
            })
            .width(Length::Fill)
            .height(Length::Fixed(92.0)),
        )
        .height(Length::Fixed(92.0))
        .style(surface_style);
        let mut inspector = column![
            crate::theme::label("INSPECTOR"),
            text("MAP").size(11).color(DIM),
            mini_map,
        ]
        .spacing(6);
        match &self.selected {
            Some(Sel::Agent(id)) => {
                inspector = inspector.push(text(format!("Agent {id}")).size(12).color(ACCENT));
                // what the node reads and writes, from its arrows (edit them with Connect)
                let key = format!("agent:{id}");
                let index = self.graph.nodes.iter().position(|n| n.key == key);
                let topics = |reads: bool| -> String {
                    self.graph
                        .edges
                        .iter()
                        .filter(|edge| Some(if reads { edge.to } else { edge.from }) == index)
                        .map(|edge| &self.graph.nodes[if reads { edge.from } else { edge.to }])
                        .filter(|n| n.kind == shepherd_app_core::pattern_graph::NodeKind::Topic)
                        .map(|n| n.label.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                inspector = inspector
                    .push(self.labelled("node", Field::Agent(AgentField::Id)))
                    .push(self.labelled("role", Field::Agent(AgentField::Role)))
                    .push(self.labelled("replicas", Field::Agent(AgentField::Count)))
                    .push(readonly("subscribe", topics(true)))
                    .push(readonly("publish", topics(false)));
                for (label, f) in [
                    ("vendor", AgentField::Vendor),
                    ("model", AgentField::Model),
                    ("level", AgentField::Level),
                    ("group", AgentField::Group),
                ] {
                    inspector = inspector.push(self.labelled(label, Field::Agent(f)));
                }
                let start = self
                    .editor
                    .pattern
                    .agents
                    .iter()
                    .find(|a| &a.id == id)
                    .map(|a| a.start)
                    .unwrap_or(false);
                inspector = inspector.push(
                    checkbox(start)
                        .label("starts the run")
                        .on_toggle(move |on| edit(EditMsg::Start(on)))
                        .size(14)
                        .text_size(12),
                );
            }
            Some(Sel::Topic(name)) => {
                inspector = inspector.push(text(format!("Topic #{name}")).size(12).color(ACCENT));
                inspector = inspector
                    .push(self.labelled("name", Field::TopicName))
                    .push(self.labelled("partitions", Field::Partitions));
            }
            None => {}
        }
        inspector = inspector
            .push(self.labelled("pattern", Field::PatternName))
            .push(self.labelled("description", Field::Description))
            .push(
                text("Click a node to edit it. Drag nodes to arrange them.")
                    .size(11)
                    .color(DIM),
            );
        let canvas = container(
            scrollable(
                Canvas::new(crate::graph_view::GraphView {
                    graph: &self.graph,
                    selected: self.selected.as_ref().map(Sel::key),
                    connecting: self.connecting.as_ref().map(Sel::key),
                    zoom: self.zoom,
                    frame,
                    mini: false,
                })
                .width(Length::Fixed(self.graph.width.max(560.0) * self.zoom))
                .height(Length::Fixed(self.graph.height.max(360.0) * self.zoom)),
            )
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::default(),
                horizontal: scrollable::Scrollbar::default(),
            })
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(8)
        .style(crate::theme::graph_style);
        let save = button(
            container(text("Save").size(13))
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
        .width(Length::Fill)
        .height(34)
        .style(primary_button_style())
        .on_press(edit(EditMsg::Save));
        let inspector = container(
            column![
                scrollable(container(inspector).padding(crate::theme::pad(0.0, 14.0, 0.0, 0.0)))
                    .height(Length::Fill),
                save
            ]
            .spacing(10),
        )
        .width(290)
        .height(Length::Fill)
        .padding(12)
        .style(crate::theme::card_style);
        col = col.push(row![canvas, inspector].spacing(14).height(Length::Fill));
        col.height(Length::Fill).into()
    }
}

/// A label with a read-only value shown like an input.
fn readonly<'a>(label: &'static str, value: String) -> Element<'a, Message> {
    row![
        container(text(label).size(12).color(DIM)).width(90),
        container(
            text(if value.is_empty() {
                "—".to_owned()
            } else {
                value
            })
            .size(13)
        )
        .padding([8, 10])
        .width(Length::Fill)
        .style(crate::theme::field_style),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}
