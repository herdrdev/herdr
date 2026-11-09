//! Pattern interactive editor state, input handling, and pure renderer.

use crate::shepherd::ui::components::list_view::text_width;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::diagram::generate_topology_diagram;
use crate::app::state::Palette;
use crate::shepherd::ui::components::list_view::put_str;
use shepherd_core::patterns::{
    get_builtin_pattern, validate, Pattern, PatternAgent, PatternKind, PatternSource, Problem,
    Topic,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatternEditorHits {
    pub tabs: Vec<(Rect, usize)>,
    pub field_rects: Vec<(Rect, usize)>,
    pub agent_tabs: Vec<(Rect, usize)>,
    pub topic_tabs: Vec<(Rect, usize)>,
    pub add_agent_button: Option<Rect>,
    pub remove_agent_button: Option<Rect>,
    pub move_agent_up_button: Option<Rect>,
    pub move_agent_down_button: Option<Rect>,
    pub add_topic_button: Option<Rect>,
    pub remove_topic_button: Option<Rect>,
    pub submit_button: Option<Rect>,
    pub cancel_button: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EditorSection {
    #[default]
    Metadata,
    Agents,
    Topics,
    Topology,
}

impl EditorSection {
    pub fn index(&self) -> usize {
        match self {
            EditorSection::Metadata => 0,
            EditorSection::Agents => 1,
            EditorSection::Topics => 2,
            EditorSection::Topology => 3,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i {
            0 => EditorSection::Metadata,
            1 => EditorSection::Agents,
            2 => EditorSection::Topics,
            _ => EditorSection::Topology,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternEditorState {
    pub original_name: String,
    pub is_new: bool,
    pub name: String,
    pub kind: PatternKind,
    pub description: String,
    pub is_global: bool,
    pub topics: Vec<Topic>,
    pub agents: Vec<PatternAgent>,
    pub active_section: EditorSection,
    pub active_field: usize,
    pub selected_agent: usize,
    pub selected_topic: usize,
    pub problems: Vec<Problem>,
    pub scroll: usize,
}

impl PatternEditorState {
    pub fn from_pattern(pattern: &Pattern, source: PatternSource) -> Self {
        let is_global = source == PatternSource::Global;
        let mut s = Self {
            original_name: pattern.name.clone(),
            is_new: false,
            name: pattern.name.clone(),
            kind: pattern.kind,
            description: pattern.description.clone(),
            is_global,
            topics: pattern.topics.clone(),
            agents: pattern.agents.clone(),
            active_section: EditorSection::Metadata,
            active_field: 0,
            selected_agent: 0,
            selected_topic: 0,
            problems: Vec::new(),
            scroll: 0,
        };
        s.revalidate();
        s
    }

    pub fn new_empty(kind: PatternKind, name: &str, is_global: bool) -> Self {
        let base = get_builtin_pattern(kind.as_str()).unwrap_or_else(|| Pattern {
            name: name.to_string(),
            kind,
            description: format!("Multi-agent pattern of type {kind}"),
            topics: vec![Topic {
                name: "events".into(),
                partitions: 1,
                key: None,
                filter: None,
            }],
            agents: vec![PatternAgent {
                id: "agent-1".into(),
                role: "brain".into(),
                vendor: None,
                model: None,
                prompt: None,
                count: 1,
                level: None,
                publishes: vec!["events".into()],
                consumes: vec![],
                group: None,
                start: false,
            }],
        });

        let mut s = Self {
            original_name: String::new(),
            is_new: true,
            name: name.to_string(),
            kind,
            description: base.description,
            is_global,
            topics: base.topics,
            agents: base.agents,
            active_section: EditorSection::Metadata,
            active_field: 0,
            selected_agent: 0,
            selected_topic: 0,
            problems: Vec::new(),
            scroll: 0,
        };
        s.revalidate();
        s
    }

    pub fn to_pattern(&self) -> Pattern {
        Pattern {
            name: self.name.clone(),
            kind: self.kind,
            description: self.description.clone(),
            topics: self.topics.clone(),
            agents: self.agents.clone(),
        }
    }

    pub fn revalidate(&mut self) {
        let config = shepherd_core::config::Config::default();
        let pat = self.to_pattern();
        self.problems = validate(&pat, &config);
    }

    pub fn is_valid(&self) -> bool {
        !self.problems.iter().any(|p| p.is_error())
    }

    pub fn field_problem(&self, path_prefix: &str) -> Option<&Problem> {
        self.problems
            .iter()
            .find(|p| p.path == path_prefix || p.path.starts_with(path_prefix))
    }

    pub fn cycle_forward(&mut self) {
        let count = self.field_count_for_section();
        if count > 0 {
            self.active_field = (self.active_field + 1) % count;
        }
    }

    pub fn cycle_backward(&mut self) {
        let count = self.field_count_for_section();
        if count > 0 {
            self.active_field = if self.active_field == 0 {
                count - 1
            } else {
                self.active_field - 1
            };
        }
    }

    pub fn field_count_for_section(&self) -> usize {
        match self.active_section {
            EditorSection::Metadata => 4, // 0: Name, 1: Kind, 2: Target, 3: Description
            EditorSection::Agents => 11, // 0: id, 1: role, 2: vendor, 3: model, 4: count, 5: level, 6: group, 7: pub, 8: sub, 9: prompt, 10: start
            EditorSection::Topics => 4,  // 0: name, 1: partitions, 2: key, 3: filter
            EditorSection::Topology => 0,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        match self.active_section {
            EditorSection::Metadata => match self.active_field {
                0 => {
                    self.name.push(c);
                    self.revalidate();
                }
                3 => {
                    self.description.push(c);
                    self.revalidate();
                }
                _ => {}
            },
            EditorSection::Agents => {
                if let Some(agent) = self.agents.get_mut(self.selected_agent) {
                    match self.active_field {
                        0 => agent.id.push(c),
                        1 => agent.role.push(c),
                        2 => {
                            let mut v = agent.vendor.clone().unwrap_or_default();
                            v.push(c);
                            agent.vendor = Some(v);
                        }
                        3 => {
                            let mut m = agent.model.clone().unwrap_or_default();
                            m.push(c);
                            agent.model = Some(m);
                        }
                        4 => {
                            if c.is_ascii_digit() {
                                let mut s = agent.count.to_string();
                                s.push(c);
                                if let Ok(val) = s.parse::<u32>() {
                                    agent.count = val.min(32);
                                }
                            }
                        }
                        5 => {
                            if c.is_ascii_digit() {
                                let mut s = agent.level.map(|l| l.to_string()).unwrap_or_default();
                                s.push(c);
                                if let Ok(val) = s.parse::<u32>() {
                                    agent.level = Some(val);
                                }
                            }
                        }
                        6 => {
                            let mut g = agent.group.clone().unwrap_or_default();
                            g.push(c);
                            agent.group = Some(g);
                        }
                        7 => {
                            let mut s = agent.publishes.join(", ");
                            s.push(c);
                            agent.publishes = s
                                .split(',')
                                .map(|x| x.trim().to_string())
                                .filter(|x| !x.is_empty())
                                .collect();
                        }
                        8 => {
                            let mut s = agent.consumes.join(", ");
                            s.push(c);
                            agent.consumes = s
                                .split(',')
                                .map(|x| x.trim().to_string())
                                .filter(|x| !x.is_empty())
                                .collect();
                        }
                        9 => {
                            let mut p = agent.prompt.clone().unwrap_or_default();
                            p.push(c);
                            agent.prompt = Some(p);
                        }
                        _ => {}
                    }
                    self.revalidate();
                }
            }
            EditorSection::Topics => {
                if let Some(topic) = self.topics.get_mut(self.selected_topic) {
                    match self.active_field {
                        0 => topic.name.push(c),
                        1 => {
                            if c.is_ascii_digit() {
                                let mut s = topic.partitions.to_string();
                                s.push(c);
                                if let Ok(val) = s.parse::<u32>() {
                                    topic.partitions = val.min(256);
                                }
                            }
                        }
                        2 => {
                            let mut k = topic.key.clone().unwrap_or_default();
                            k.push(c);
                            topic.key = Some(k);
                        }
                        3 => {
                            let mut f = topic.filter.clone().unwrap_or_default();
                            f.push(c);
                            topic.filter = Some(f);
                        }
                        _ => {}
                    }
                    self.revalidate();
                }
            }
            EditorSection::Topology => {}
        }
    }

    pub fn delete_char(&mut self) {
        match self.active_section {
            EditorSection::Metadata => match self.active_field {
                0 => {
                    self.name.pop();
                    self.revalidate();
                }
                3 => {
                    self.description.pop();
                    self.revalidate();
                }
                _ => {}
            },
            EditorSection::Agents => {
                if let Some(agent) = self.agents.get_mut(self.selected_agent) {
                    match self.active_field {
                        0 => {
                            agent.id.pop();
                        }
                        1 => {
                            agent.role.pop();
                        }
                        2 => {
                            if let Some(ref mut v) = agent.vendor {
                                v.pop();
                                if v.is_empty() {
                                    agent.vendor = None;
                                }
                            }
                        }
                        3 => {
                            if let Some(ref mut m) = agent.model {
                                m.pop();
                                if m.is_empty() {
                                    agent.model = None;
                                }
                            }
                        }
                        4 => {
                            let mut s = agent.count.to_string();
                            s.pop();
                            agent.count = s.parse::<u32>().unwrap_or(1).max(1);
                        }
                        5 => {
                            if let Some(ref mut l) = agent.level {
                                let mut s = l.to_string();
                                s.pop();
                                agent.level = s.parse::<u32>().ok();
                            }
                        }
                        6 => {
                            if let Some(ref mut g) = agent.group {
                                g.pop();
                                if g.is_empty() {
                                    agent.group = None;
                                }
                            }
                        }
                        7 => {
                            let mut s = agent.publishes.join(", ");
                            s.pop();
                            agent.publishes = s
                                .split(',')
                                .map(|x| x.trim().to_string())
                                .filter(|x| !x.is_empty())
                                .collect();
                        }
                        8 => {
                            let mut s = agent.consumes.join(", ");
                            s.pop();
                            agent.consumes = s
                                .split(',')
                                .map(|x| x.trim().to_string())
                                .filter(|x| !x.is_empty())
                                .collect();
                        }
                        9 => {
                            if let Some(ref mut p) = agent.prompt {
                                p.pop();
                                if p.is_empty() {
                                    agent.prompt = None;
                                }
                            }
                        }
                        _ => {}
                    }
                    self.revalidate();
                }
            }
            EditorSection::Topics => {
                if let Some(topic) = self.topics.get_mut(self.selected_topic) {
                    match self.active_field {
                        0 => {
                            topic.name.pop();
                        }
                        1 => {
                            let mut s = topic.partitions.to_string();
                            s.pop();
                            topic.partitions = s.parse::<u32>().unwrap_or(1).max(1);
                        }
                        2 => {
                            if let Some(ref mut k) = topic.key {
                                k.pop();
                                if k.is_empty() {
                                    topic.key = None;
                                }
                            }
                        }
                        3 => {
                            if let Some(ref mut f) = topic.filter {
                                f.pop();
                                if f.is_empty() {
                                    topic.filter = None;
                                }
                            }
                        }
                        _ => {}
                    }
                    self.revalidate();
                }
            }
            EditorSection::Topology => {}
        }
    }

    pub fn toggle_active_choice(&mut self) {
        if self.active_section == EditorSection::Agents && self.active_field == 10 {
            if let Some(agent) = self.agents.get_mut(self.selected_agent) {
                agent.start = !agent.start;
                self.revalidate();
            }
            return;
        }
        if self.active_section == EditorSection::Metadata {
            match self.active_field {
                1 => {
                    self.kind = match self.kind {
                        PatternKind::OrchestratorWorker => PatternKind::Hierarchical,
                        PatternKind::Hierarchical => PatternKind::Blackboard,
                        PatternKind::Blackboard => PatternKind::Market,
                        PatternKind::Market => PatternKind::OrchestratorWorker,
                    };
                    self.revalidate();
                }
                2 => {
                    self.is_global = !self.is_global;
                    self.revalidate();
                }
                _ => {}
            }
        }
    }

    pub fn add_agent(&mut self) {
        let new_id = format!("agent-{}", self.agents.len() + 1);
        let first_topic = self
            .topics
            .first()
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "events".into());
        self.agents.push(PatternAgent {
            id: new_id,
            role: "coder".into(),
            vendor: None,
            model: None,
            prompt: None,
            count: 1,
            level: if self.kind == PatternKind::Hierarchical {
                Some(1)
            } else {
                None
            },
            publishes: vec![first_topic],
            consumes: vec![],
            group: None,
            start: false,
        });
        self.selected_agent = self.agents.len() - 1;
        self.revalidate();
    }

    pub fn remove_agent(&mut self) {
        if self.agents.len() > 1 && self.selected_agent < self.agents.len() {
            self.agents.remove(self.selected_agent);
            if self.selected_agent >= self.agents.len() {
                self.selected_agent = self.agents.len() - 1;
            }
            self.revalidate();
        }
    }

    pub fn move_agent_up(&mut self) {
        if self.selected_agent > 0 && self.selected_agent < self.agents.len() {
            self.agents
                .swap(self.selected_agent, self.selected_agent - 1);
            self.selected_agent -= 1;
            self.revalidate();
        }
    }

    pub fn move_agent_down(&mut self) {
        if self.selected_agent + 1 < self.agents.len() {
            self.agents
                .swap(self.selected_agent, self.selected_agent + 1);
            self.selected_agent += 1;
            self.revalidate();
        }
    }

    pub fn add_topic(&mut self) {
        let new_name = format!("topic-{}", self.topics.len() + 1);
        self.topics.push(Topic {
            name: new_name,
            partitions: 1,
            key: None,
            filter: None,
        });
        self.selected_topic = self.topics.len() - 1;
        self.revalidate();
    }

    pub fn remove_topic(&mut self) {
        if self.topics.len() > 1 && self.selected_topic < self.topics.len() {
            self.topics.remove(self.selected_topic);
            if self.selected_topic >= self.topics.len() {
                self.selected_topic = self.topics.len() - 1;
            }
            self.revalidate();
        }
    }
}

pub fn render_pattern_editor(
    buffer: &mut Buffer,
    area: Rect,
    editor: &PatternEditorState,
    palette: &Palette,
) -> PatternEditorHits {
    let mut hits = PatternEditorHits::default();
    if area.width < 15 || area.height < 6 {
        return hits;
    }

    let mut current_y = area.y;

    // 1. Editor Title
    let title = format!("PATTERN EDITOR: {}", editor.name);
    put_str(
        buffer,
        area.x,
        current_y,
        area.width,
        &title,
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    current_y += 1;

    // 2. Section navigation tabs
    let tabs = ["1. Metadata", "2. Agents", "3. Topics", "4. Topology"];
    let mut tab_x = area.x;
    for (i, t_label) in tabs.iter().enumerate() {
        let is_active = editor.active_section.index() == i;
        let style = if is_active {
            Style::default()
                .fg(palette.panel_bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0).bg(palette.surface0)
        };
        let formatted = format!(" [ {} ] ", t_label);
        let len = text_width(&formatted);
        if tab_x + len < area.right() {
            put_str(buffer, tab_x, current_y, len, &formatted, style);
            hits.tabs.push((Rect::new(tab_x, current_y, len, 1), i));
            tab_x += len + 1;
        }
    }
    current_y += 2;

    // Divider
    let sep = "─".repeat(area.width as usize);
    put_str(
        buffer,
        area.x,
        current_y - 1,
        area.width,
        &sep,
        Style::default().fg(palette.surface_dim),
    );

    // 3. Content per active section
    match editor.active_section {
        EditorSection::Metadata => {
            let fields_meta = [
                ("Pattern name:", editor.name.as_str()),
                ("Topology type (Space changes):", editor.kind.as_str()),
                (
                    "Save scope (Space changes):",
                    if editor.is_global {
                        "Global (~/.config)"
                    } else {
                        "Proyecto (.shepherd)"
                    },
                ),
                ("Description:", editor.description.as_str()),
            ];

            for (i, (label, val)) in fields_meta.iter().enumerate() {
                if current_y + 2 >= area.bottom().saturating_sub(4) {
                    break;
                }
                let is_active = i == editor.active_field;
                let prob = match i {
                    0 => editor.field_problem("name"),
                    _ => None,
                };

                let label_style = if is_active {
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0)
                };

                let mut full_label = label.to_string();
                if let Some(p) = prob {
                    full_label.push_str(&format!("  [✗ {}]", p.message));
                }
                put_str(
                    buffer,
                    area.x,
                    current_y,
                    area.width,
                    &full_label,
                    label_style,
                );
                current_y += 1;

                let box_w = area.width.saturating_sub(2);
                let box_rect = Rect::new(area.x, current_y, box_w, 1);
                hits.field_rects.push((box_rect, i));

                let box_bg = if is_active {
                    palette.surface0
                } else {
                    palette.panel_bg
                };
                let val_style = Style::default().fg(palette.accent).bg(box_bg);

                let display_val = if is_active {
                    format!("{}█", val)
                } else if val.is_empty() {
                    "(empty)".into()
                } else {
                    val.to_string()
                };
                put_str(buffer, area.x, current_y, box_w, &display_val, val_style);
                current_y += 2;
            }
        }
        EditorSection::Agents => {
            let mut ag_x = area.x;
            for (i, ag) in editor.agents.iter().enumerate() {
                let is_sel = i == editor.selected_agent;
                let st = if is_sel {
                    Style::default()
                        .fg(palette.panel_bg)
                        .bg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0).bg(palette.surface0)
                };
                let txt = format!(" [{}: {}] ", ag.id, ag.role);
                let len = text_width(&txt);
                if ag_x + len < area.right().saturating_sub(25) {
                    put_str(buffer, ag_x, current_y, len, &txt, st);
                    hits.agent_tabs
                        .push((Rect::new(ag_x, current_y, len, 1), i));
                    ag_x += len + 1;
                }
            }

            let add_rect = Rect::new(area.right().saturating_sub(24), current_y, 5, 1);
            put_str(
                buffer,
                add_rect.x,
                add_rect.y,
                5,
                "[ + ]",
                Style::default().fg(palette.green),
            );
            hits.add_agent_button = Some(add_rect);

            let rem_rect = Rect::new(area.right().saturating_sub(18), current_y, 5, 1);
            put_str(
                buffer,
                rem_rect.x,
                rem_rect.y,
                5,
                "[ - ]",
                Style::default().fg(palette.red),
            );
            hits.remove_agent_button = Some(rem_rect);

            let up_rect = Rect::new(area.right().saturating_sub(12), current_y, 5, 1);
            put_str(
                buffer,
                up_rect.x,
                up_rect.y,
                5,
                "[ ↑ ]",
                Style::default().fg(palette.accent),
            );
            hits.move_agent_up_button = Some(up_rect);

            let dn_rect = Rect::new(area.right().saturating_sub(6), current_y, 5, 1);
            put_str(
                buffer,
                dn_rect.x,
                dn_rect.y,
                5,
                "[ ↓ ]",
                Style::default().fg(palette.accent),
            );
            hits.move_agent_down_button = Some(dn_rect);

            current_y += 2;

            if let Some(agent) = editor.agents.get(editor.selected_agent) {
                let count_str = agent.count.to_string();
                let lvl_str = agent.level.map(|l| l.to_string()).unwrap_or_default();
                let pub_str = agent.publishes.join(", ");
                let sub_str = agent.consumes.join(", ");

                let fields_agent = [
                    ("Agent ID:", agent.id.as_str()),
                    (
                        "Role (brain, coder, reviewer, tester, docs):",
                        agent.role.as_str(),
                    ),
                    (
                        "Vendor (optional: claude, codex, gemini):",
                        agent.vendor.as_deref().unwrap_or(""),
                    ),
                    ("Model (optional):", agent.model.as_deref().unwrap_or("")),
                    ("Instancias (count):", count_str.as_str()),
                    (
                        "Hierarchy level (level, hierarchical only):",
                        lvl_str.as_str(),
                    ),
                    (
                        "Consumer group (group):",
                        agent.group.as_deref().unwrap_or(""),
                    ),
                    ("Topics it publishes (comma):", pub_str.as_str()),
                    ("Topics it subscribes to (comma):", sub_str.as_str()),
                    (
                        "Prompt / Persona (personality in natural language):",
                        agent.prompt.as_deref().unwrap_or(""),
                    ),
                    (
                        "Starts the pattern (start; Space toggles):",
                        if agent.start { "yes" } else { "no" },
                    ),
                ];

                for (i, (label, val)) in fields_agent.iter().enumerate() {
                    if current_y + 1 >= area.bottom().saturating_sub(3) {
                        break;
                    }
                    let is_active = i == editor.active_field;
                    let path = format!(
                        "agents[{}].{}",
                        editor.selected_agent,
                        match i {
                            0 => "id",
                            1 => "role",
                            2 => "vendor",
                            3 => "model",
                            4 => "count",
                            5 => "level",
                            6 => "group",
                            7 => "publishes",
                            8 => "consumes",
                            10 => "start",
                            _ => "prompt",
                        }
                    );
                    let prob = editor.field_problem(&path);

                    let label_style = if is_active {
                        Style::default()
                            .fg(palette.accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette.overlay0)
                    };

                    let mut full_label = label.to_string();
                    if let Some(p) = prob {
                        full_label.push_str(&format!("  [✗ {}]", p.message));
                    }
                    put_str(
                        buffer,
                        area.x,
                        current_y,
                        area.width,
                        &full_label,
                        label_style,
                    );
                    current_y += 1;

                    let box_w = area.width.saturating_sub(2);
                    let box_rect = Rect::new(area.x, current_y, box_w, 1);
                    hits.field_rects.push((box_rect, i));

                    let box_bg = if is_active {
                        palette.surface0
                    } else {
                        palette.panel_bg
                    };
                    let val_style = Style::default().fg(palette.accent).bg(box_bg);

                    let display_val = if is_active {
                        format!("{}█", val)
                    } else if val.is_empty() {
                        "(empty / default)".into()
                    } else {
                        val.to_string()
                    };
                    put_str(buffer, area.x, current_y, box_w, &display_val, val_style);
                    current_y += 1;
                }
            }
        }
        EditorSection::Topics => {
            let mut top_x = area.x;
            for (i, top) in editor.topics.iter().enumerate() {
                let is_sel = i == editor.selected_topic;
                let st = if is_sel {
                    Style::default()
                        .fg(palette.panel_bg)
                        .bg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0).bg(palette.surface0)
                };
                let txt = format!(" [{}: (p:{})] ", top.name, top.partitions);
                let len = text_width(&txt);
                if top_x + len < area.right().saturating_sub(14) {
                    put_str(buffer, top_x, current_y, len, &txt, st);
                    hits.topic_tabs
                        .push((Rect::new(top_x, current_y, len, 1), i));
                    top_x += len + 1;
                }
            }

            let add_rect = Rect::new(area.right().saturating_sub(13), current_y, 5, 1);
            put_str(
                buffer,
                add_rect.x,
                add_rect.y,
                5,
                "[ + ]",
                Style::default().fg(palette.green),
            );
            hits.add_topic_button = Some(add_rect);

            let rem_rect = Rect::new(area.right().saturating_sub(7), current_y, 5, 1);
            put_str(
                buffer,
                rem_rect.x,
                rem_rect.y,
                5,
                "[ - ]",
                Style::default().fg(palette.red),
            );
            hits.remove_topic_button = Some(rem_rect);

            current_y += 2;

            if let Some(topic) = editor.topics.get(editor.selected_topic) {
                let p_str = topic.partitions.to_string();
                let fields_topic = [
                    ("Topic name:", topic.name.as_str()),
                    ("Number of partitions (1..256):", p_str.as_str()),
                    (
                        "Partition key field (optional):",
                        topic.key.as_deref().unwrap_or(""),
                    ),
                    (
                        "Topic filter (optional):",
                        topic.filter.as_deref().unwrap_or(""),
                    ),
                ];

                for (i, (label, val)) in fields_topic.iter().enumerate() {
                    if current_y + 2 >= area.bottom().saturating_sub(3) {
                        break;
                    }
                    let is_active = i == editor.active_field;
                    let path = format!(
                        "topics[{}].{}",
                        editor.selected_topic,
                        match i {
                            0 => "name",
                            1 => "partitions",
                            2 => "key",
                            _ => "filter",
                        }
                    );
                    let prob = editor.field_problem(&path);

                    let label_style = if is_active {
                        Style::default()
                            .fg(palette.accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette.overlay0)
                    };

                    let mut full_label = label.to_string();
                    if let Some(p) = prob {
                        full_label.push_str(&format!("  [✗ {}]", p.message));
                    }
                    put_str(
                        buffer,
                        area.x,
                        current_y,
                        area.width,
                        &full_label,
                        label_style,
                    );
                    current_y += 1;

                    let box_w = area.width.saturating_sub(2);
                    let box_rect = Rect::new(area.x, current_y, box_w, 1);
                    hits.field_rects.push((box_rect, i));

                    let box_bg = if is_active {
                        palette.surface0
                    } else {
                        palette.panel_bg
                    };
                    let val_style = Style::default().fg(palette.accent).bg(box_bg);

                    let display_val = if is_active {
                        format!("{}█", val)
                    } else if val.is_empty() {
                        "(empty)".into()
                    } else {
                        val.to_string()
                    };
                    put_str(buffer, area.x, current_y, box_w, &display_val, val_style);
                    current_y += 2;
                }
            }
        }
        EditorSection::Topology => {
            let pat = editor.to_pattern();
            let diag_lines = generate_topology_diagram(&pat, area.width.saturating_sub(2) as usize);
            for line in diag_lines {
                if current_y >= area.bottom().saturating_sub(3) {
                    break;
                }
                put_str(
                    buffer,
                    area.x,
                    current_y,
                    area.width,
                    &line,
                    Style::default().fg(palette.accent),
                );
                current_y += 1;
            }
        }
    }

    // 4. Live validation feedback banner & bottom action buttons
    let bottom_y = area.bottom().saturating_sub(2);
    let error_count = editor.problems.iter().filter(|p| p.is_error()).count();
    let warn_count = editor.problems.iter().filter(|p| !p.is_error()).count();

    if error_count == 0 && warn_count == 0 {
        let ok_msg = "✓ Valid pattern (0 errors, 0 warnings) - ready to save";
        put_str(
            buffer,
            area.x,
            bottom_y,
            area.width,
            ok_msg,
            Style::default().fg(palette.green),
        );
    } else if error_count > 0 {
        let first_err = editor
            .problems
            .iter()
            .find(|p| p.is_error())
            .map(|p| p.message.as_str())
            .unwrap_or("");
        let err_msg = format!("✗ {} error(s): {}", error_count, first_err);
        put_str(
            buffer,
            area.x,
            bottom_y,
            area.width,
            &err_msg,
            Style::default()
                .fg(palette.red)
                .add_modifier(Modifier::BOLD),
        );
    } else {
        let first_warn = editor
            .problems
            .iter()
            .find(|p| !p.is_error())
            .map(|p| p.message.as_str())
            .unwrap_or("");
        let warn_msg = format!("⚠ {} aviso(s): {}", warn_count, first_warn);
        put_str(
            buffer,
            area.x,
            bottom_y,
            area.width,
            &warn_msg,
            Style::default().fg(palette.yellow),
        );
    }

    // Buttons
    let btn_y = area.bottom().saturating_sub(1);
    let submit_label = if editor.is_valid() {
        "[ Save (Enter) ]"
    } else {
        "[ Save blocked ]"
    };
    let submit_len = text_width(&submit_label);
    let submit_rect = Rect::new(area.x, btn_y, submit_len, 1);
    let submit_style = if editor.is_valid() {
        Style::default()
            .fg(palette.panel_bg)
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.red).bg(palette.surface0)
    };
    put_str(
        buffer,
        submit_rect.x,
        submit_rect.y,
        submit_len,
        submit_label,
        submit_style,
    );
    hits.submit_button = Some(submit_rect);

    let cancel_label = "[ Cancel (Esc) ]";
    let cancel_len = text_width(&cancel_label);
    let cancel_rect = Rect::new(submit_rect.right() + 2, btn_y, cancel_len, 1);
    put_str(
        buffer,
        cancel_rect.x,
        cancel_rect.y,
        cancel_len,
        cancel_label,
        Style::default().fg(palette.overlay0),
    );
    hits.cancel_button = Some(cancel_rect);

    hits
}
