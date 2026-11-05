use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
use super::components::{
    confirm_stripe::{render_confirm_stripe, ConfirmStripeHits},
    detail_view::{
        render_detail_view, AlertLevel, DetailContentBlock, DetailHeader, DetailViewHits,
    },
    empty_state::{render_empty_state, EmptyStateHits},
    inline_form::{render_inline_form, FormField, InlineFormHits, InlineFormState},
    list_view::{put_str, render_list_view, ListViewHits, ListViewItem},
};

#[allow(unused_imports)]
pub use shepherd_core::patterns::{
    get_builtin_pattern, get_builtin_template, is_builtin_template, list_patterns, validate,
    Pattern, PatternAgent, PatternDetail, PatternKind, PatternSource, PatternSummary, Problem,
    Severity, Topic,
};

// ============================================================================
// Topology Diagram Generation (Pure Function)
// ============================================================================

/// Pure function to generate an ASCII topology diagram for a pattern.
/// Adapts layout based on `max_width`.
pub fn generate_topology_diagram(pattern: &Pattern, max_width: usize) -> Vec<String> {
    let w = max_width.max(20);
    let narrow = w < 50;

    match pattern.kind {
        PatternKind::OrchestratorWorker => render_orchestrator_worker_diagram(pattern, narrow, w),
        PatternKind::Hierarchical => render_hierarchical_diagram(pattern, narrow, w),
        PatternKind::Blackboard => render_blackboard_diagram(pattern, narrow, w),
        PatternKind::Market => render_market_diagram(pattern, narrow, w),
    }
}

fn truncate_line(s: &str, max_w: usize) -> String {
    if s.chars().count() <= max_w {
        s.to_string()
    } else {
        let mut res = String::new();
        for ch in s.chars().take(max_w.saturating_sub(1)) {
            res.push(ch);
        }
        res.push('…');
        res
    }
}

fn box_line(text: &str, width: usize) -> (String, String, String) {
    let inner_w = width.saturating_sub(2).max(10);
    let top = format!("┌{}┐", "─".repeat(inner_w));
    let bot = format!("└{}┘", "─".repeat(inner_w));
    let truncated_text = truncate_line(text, inner_w);
    let padding = inner_w.saturating_sub(truncated_text.chars().count());
    let left_pad = padding / 2;
    let right_pad = padding - left_pad;
    let mid = format!("│{}{}{}│", " ".repeat(left_pad), truncated_text, " ".repeat(right_pad));
    (top, mid, bot)
}

fn render_orchestrator_worker_diagram(pattern: &Pattern, narrow: bool, max_w: usize) -> Vec<String> {
    let mut lines = Vec::new();

    // Identify orchestrator and worker
    let orchestrator = pattern.agents.iter().find(|a| {
        a.role.to_lowercase() == "brain"
            || a.role.to_lowercase() == "planner"
            || a.group.is_none()
    }).or_else(|| pattern.agents.first());

    let worker = pattern.agents.iter().find(|a| {
        a.role.to_lowercase() == "coder" || a.group.is_some() || a.count > 1
    }).or_else(|| pattern.agents.get(1));

    let orch_role = orchestrator.map(|a| a.role.as_str()).unwrap_or("brain");
    let orch_count = orchestrator.map(|a| a.count).unwrap_or(1);
    let orch_pub = orchestrator.and_then(|a| a.publishes.first().cloned()).unwrap_or_else(|| "orders".into());
    let orch_sub = orchestrator.and_then(|a| a.consumes.first().cloned()).unwrap_or_else(|| "results".into());

    let worker_role = worker.map(|a| a.role.as_str()).unwrap_or("coder");
    let worker_count = worker.map(|a| a.count).unwrap_or(3);
    let worker_group = worker.and_then(|a| a.group.as_deref()).unwrap_or("worker-pool");

    let orders_topic = pattern.topics.iter().find(|t| t.name == orch_pub);
    let orders_p = orders_topic.map(|t| t.partitions).unwrap_or(4);

    let results_topic = pattern.topics.iter().find(|t| t.name == orch_sub);
    let results_p = results_topic.map(|t| t.partitions).unwrap_or(1);

    if narrow {
        lines.push(format!("[Orquestador] {orch_role} (x{orch_count})"));
        lines.push(format!("  │ publica: {orch_pub}"));
        lines.push("  ▼".into());
        lines.push(format!("[Tópico: {orch_pub} (p:{orders_p})]"));
        lines.push(format!("  │ consume: {worker_group}"));
        lines.push("  ▼".into());
        lines.push(format!("[Workers] {worker_role} (x{worker_count})"));
        lines.push(format!("  │ publica: {orch_sub}"));
        lines.push("  ▼".into());
        lines.push(format!("[Tópico: {orch_sub} (p:{results_p})]"));
        lines.push("  │ consume".into());
        lines.push("  ▼".into());
        lines.push(format!("[Orquestador: {orch_role}] (cierra bucle)"));
    } else {
        let box_w = max_w.min(46).max(34);
        let orch_txt = format!("[Orquestador] {} (x{})", orch_role, orch_count);
        let (t1, m1, b1) = box_line(&orch_txt, box_w);
        lines.push(t1);
        lines.push(m1);
        lines.push(b1);
        let center = box_w / 2;
        let indent = " ".repeat(center.saturating_sub(1));
        lines.push(format!("{}│ publica: {}", indent, orch_pub));
        lines.push(format!("{}▼", indent));

        let top_txt = format!("[Tópico: {} (p:{})]", orch_pub, orders_p);
        let top_pad = " ".repeat(center.saturating_sub(top_txt.chars().count() / 2));
        lines.push(format!("{}{}", top_pad, top_txt));
        lines.push(format!("{}│ consume: {}", indent, worker_group));
        lines.push(format!("{}▼", indent));

        let worker_txt = format!("[Worker Pool] {} (x{}) [group: {}]", worker_role, worker_count, worker_group);
        let (t2, m2, b2) = box_line(&worker_txt, box_w);
        lines.push(t2);
        lines.push(m2);
        lines.push(b2);
        lines.push(format!("{}│ publica: {}", indent, orch_sub));
        lines.push(format!("{}▼", indent));

        let res_txt = format!("[Tópico: {} (p:{})]", orch_sub, results_p);
        let res_pad = " ".repeat(center.saturating_sub(res_txt.chars().count() / 2));
        lines.push(format!("{}{}", res_pad, res_txt));
        lines.push(format!("{}│ consume", indent));
        lines.push(format!("{}▼", indent));

        let return_txt = format!("[Orquestador: {}]", orch_role);
        let ret_pad = " ".repeat(center.saturating_sub(return_txt.chars().count() / 2));
        lines.push(format!("{}{}", ret_pad, return_txt));
    }

    lines.into_iter().map(|l| truncate_line(&l, max_w)).collect()
}

fn render_hierarchical_diagram(pattern: &Pattern, narrow: bool, max_w: usize) -> Vec<String> {
    let mut lines = Vec::new();

    // Group agents by level
    let mut levels: Vec<(u32, Vec<&PatternAgent>)> = Vec::new();
    for agent in &pattern.agents {
        let lvl = agent.level.unwrap_or(0);
        if let Some(entry) = levels.iter_mut().find(|(l, _)| *l == lvl) {
            entry.1.push(agent);
        } else {
            levels.push((lvl, vec![agent]));
        }
    }
    levels.sort_by_key(|(l, _)| *l);

    if levels.is_empty() {
        lines.push("[Jerárquico sin agentes]".into());
        return lines;
    }

    if narrow {
        for (idx, (lvl, ags)) in levels.iter().enumerate() {
            let role_desc = ags.iter().map(|a| format!("{} (x{})", a.role, a.count)).collect::<Vec<_>>().join(", ");
            let label = match lvl {
                0 => "Raíz / Director",
                1 => "Managers",
                _ => "Especialistas",
            };
            lines.push(format!("[N{}: {}] {}", lvl, label, role_desc));
            if idx + 1 < levels.len() {
                let pub_topic = ags.first().and_then(|a| a.publishes.first()).cloned().unwrap_or_else(|| "orders".into());
                lines.push(format!("  │ publica: {pub_topic}"));
                lines.push("  ▼".into());
                lines.push(format!("[Tópico: {pub_topic}]"));
                lines.push("  │ consume".into());
                lines.push("  ▼".into());
            }
        }
        lines.push("  ◄── retornos vía tópicos de resultados".into());
    } else {
        let box_w = max_w.min(52).max(36);
        for (idx, (lvl, ags)) in levels.iter().enumerate() {
            let role_desc = ags.iter().map(|a| format!("{} (x{})", a.role, a.count)).collect::<Vec<_>>().join(", ");
            let label = match lvl {
                0 => "Nivel 0: Director",
                1 => "Nivel 1: Managers",
                _ => "Nivel 2: Especialistas",
            };
            let text = format!("[{}] {}", label, role_desc);
            let (t, m, b) = box_line(&text, box_w);
            lines.push(t);
            lines.push(m);
            lines.push(b);

            if idx + 1 < levels.len() {
                let center = box_w / 2;
                let indent = " ".repeat(center.saturating_sub(1));
                let pub_topic = ags.first().and_then(|a| a.publishes.first()).cloned().unwrap_or_else(|| "cascade-orders".into());
                lines.push(format!("{}│ publica: {}", indent, pub_topic));
                lines.push(format!("{}▼", indent));

                let top_txt = format!("[Tópico: {}]", pub_topic);
                let top_pad = " ".repeat(center.saturating_sub(top_txt.chars().count() / 2));
                lines.push(format!("{}{}", top_pad, top_txt));
                lines.push(format!("{}│ consume", indent));
                lines.push(format!("{}▼", indent));
            }
        }
        lines.push("  ▲── Resultados en cascada inversa (team-results ──► root-results) ──► Director".into());
    }

    lines.into_iter().map(|l| truncate_line(&l, max_w)).collect()
}

fn render_blackboard_diagram(pattern: &Pattern, narrow: bool, max_w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let topic_name = pattern.topics.first().map(|t| t.name.as_str()).unwrap_or("blackboard");
    let partitions = pattern.topics.first().map(|t| t.partitions).unwrap_or(1);

    if narrow {
        lines.push(format!("[Pizarra: {} (p:{})]", topic_name, partitions));
        for agent in &pattern.agents {
            lines.push(format!("  ◄──► {} ({})", agent.id, agent.role));
        }
    } else {
        let box_w = max_w.min(50).max(36);
        let header = format!("PIZARRA COMPARTIDA: {} (p:{})", topic_name, partitions);
        let (t, m, b) = box_line(&header, box_w);
        lines.push(t);
        lines.push(m);
        lines.push(b);

        // Arrows line
        let mut arrows = String::new();
        let agent_count = pattern.agents.len().max(1);
        let step = box_w.saturating_sub(4) / agent_count;
        for _ in 0..agent_count {
            arrows.push_str(&" ".repeat(step.saturating_sub(8)));
            arrows.push_str("▲     ▲ ");
        }
        lines.push("    ▲               ▲               ▲".into());
        lines.push("    │ pub/sub       │ pub/sub       │ pub/sub".into());
        lines.push("    ▼               ▼               ▼".into());

        let mut ag_line = String::new();
        for ag in &pattern.agents {
            ag_line.push_str(&format!("[{}:{}] ", ag.id, ag.role));
        }
        lines.push(ag_line);
    }

    lines.into_iter().map(|l| truncate_line(&l, max_w)).collect()
}

fn render_market_diagram(pattern: &Pattern, narrow: bool, max_w: usize) -> Vec<String> {
    let mut lines = Vec::new();

    let mm = pattern.agents.iter().find(|a| a.role.to_lowercase() == "brain" || a.id.contains("market")).map(|a| a.role.as_str()).unwrap_or("brain");
    let req = pattern.agents.iter().find(|a| a.publishes.iter().any(|t| t == "ask")).map(|a| a.role.as_str()).unwrap_or("docs");
    let sol = pattern.agents.iter().find(|a| a.publishes.iter().any(|t| t == "bid")).map(|a| a.role.as_str()).unwrap_or("coder");

    if narrow {
        lines.push(format!("[Requesters: {req}]"));
        lines.push("  │ publica demanda".into());
        lines.push("  ▼".into());
        lines.push("[Tópico: ask (p:1)]".into());
        lines.push("  │ consume".into());
        lines.push("  ▼".into());
        lines.push(format!("[Solvers: {sol}]"));
        lines.push("  │ oferta competitiva".into());
        lines.push("  ▼".into());
        lines.push("[Tópico: bid (p:1)]".into());
        lines.push("  │ consume".into());
        lines.push("  ▼".into());
        lines.push(format!("[Market-Maker: {mm}]"));
        lines.push("  │ adjudica (transaction)".into());
        lines.push("  ▼".into());
        lines.push("[Tópico: transaction] ──► [Solvers / Requesters]".into());
    } else {
        lines.push(format!("[Requesters: {req}] ──────► [Tópico: ask] ──────► [Solvers: {sol}]"));
        lines.push("                                                          │ ofertas competitivas".into());
        lines.push("                                                          ▼".into());
        lines.push(format!("[Market-Maker: {mm}] ◄────────── [Tópico: bid] ───────────┘"));
        lines.push("         │ empareja y adjudica transacciones".into());
        lines.push("         ▼".into());
        lines.push("[Tópico: transaction] ──────► [Ejecución en Solvers y Requesters]".into());
    }

    lines.into_iter().map(|l| truncate_line(&l, max_w)).collect()
}

// ============================================================================
// Actions and Hits
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdPatternsAction {
    List,
    Get { name: String },
    Save { pattern: Pattern, global: bool },
    Delete { name: String },
    Validate { name: Option<String>, pattern: Option<Pattern> },
    RunPattern {
        pattern: String,
        task: Option<String>,
        worktree: Option<String>,
    },
}

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdPatternsHits {
    pub list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub editor_hits: Option<PatternEditorHits>,
    pub stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
    pub new_from_template_hits: Option<InlineFormHits>,
    pub duplicate_hits: Option<InlineFormHits>,
    pub run_form_hits: Option<InlineFormHits>,
}

// ============================================================================
// Editor & Sub-action States
// ============================================================================

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
        // Load base from builtin template if available
        let base = get_builtin_pattern(kind.as_str()).unwrap_or_else(|| Pattern {
            name: name.to_string(),
            kind,
            description: format!("Patrón multi-agente de tipo {kind}"),
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
        self.problems.iter().find(|p| p.path == path_prefix || p.path.starts_with(path_prefix))
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
            EditorSection::Agents => 10,   // 0: id, 1: role, 2: vendor, 3: model, 4: count, 5: level, 6: group, 7: pub, 8: sub, 9: prompt
            EditorSection::Topics => 4,   // 0: name, 1: partitions, 2: key, 3: filter
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
                            agent.publishes = s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                        }
                        8 => {
                            let mut s = agent.consumes.join(", ");
                            s.push(c);
                            agent.consumes = s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
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
                        0 => { agent.id.pop(); }
                        1 => { agent.role.pop(); }
                        2 => {
                            if let Some(ref mut v) = agent.vendor {
                                v.pop();
                                if v.is_empty() { agent.vendor = None; }
                            }
                        }
                        3 => {
                            if let Some(ref mut m) = agent.model {
                                m.pop();
                                if m.is_empty() { agent.model = None; }
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
                                if g.is_empty() { agent.group = None; }
                            }
                        }
                        7 => {
                            let mut s = agent.publishes.join(", ");
                            s.pop();
                            agent.publishes = s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                        }
                        8 => {
                            let mut s = agent.consumes.join(", ");
                            s.pop();
                            agent.consumes = s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                        }
                        9 => {
                            if let Some(ref mut p) = agent.prompt {
                                p.pop();
                                if p.is_empty() { agent.prompt = None; }
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
                        0 => { topic.name.pop(); }
                        1 => {
                            let mut s = topic.partitions.to_string();
                            s.pop();
                            topic.partitions = s.parse::<u32>().unwrap_or(1).max(1);
                        }
                        2 => {
                            if let Some(ref mut k) = topic.key {
                                k.pop();
                                if k.is_empty() { topic.key = None; }
                            }
                        }
                        3 => {
                            if let Some(ref mut f) = topic.filter {
                                f.pop();
                                if f.is_empty() { topic.filter = None; }
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
        let first_topic = self.topics.first().map(|t| t.name.clone()).unwrap_or_else(|| "events".into());
        self.agents.push(PatternAgent {
            id: new_id,
            role: "coder".into(),
            vendor: None,
            model: None,
            prompt: None,
            count: 1,
            level: if self.kind == PatternKind::Hierarchical { Some(1) } else { None },
            publishes: vec![first_topic],
            consumes: vec![],
            group: None,
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
            self.agents.swap(self.selected_agent, self.selected_agent - 1);
            self.selected_agent -= 1;
            self.revalidate();
        }
    }

    pub fn move_agent_down(&mut self) {
        if self.selected_agent + 1 < self.agents.len() {
            self.agents.swap(self.selected_agent, self.selected_agent + 1);
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternSubAction {
    NewFromTemplate(NewFromTemplateState),
    Duplicate(DuplicatePatternState),
    ConfirmDelete { name: String, source: PatternSource },
    Edit(PatternEditorState),
    Run(PatternRunFormState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternRunFormState {
    pub pattern_name: String,
    pub task: String,
    pub worktree: String,
    pub active_field: usize, // 0: task, 1: worktree
}

impl PatternRunFormState {
    pub fn new(pattern_name: String) -> Self {
        Self {
            pattern_name,
            task: String::new(),
            worktree: String::new(),
            active_field: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewFromTemplateState {
    pub kind_index: usize, // 0..3
    pub name: String,
    pub is_global: bool,
    pub active_field: usize, // 0: kind, 1: name, 2: target
}

impl Default for NewFromTemplateState {
    fn default() -> Self {
        Self {
            kind_index: 0,
            name: "mi-orquestador".into(),
            is_global: false,
            active_field: 1,
        }
    }
}

impl NewFromTemplateState {
    pub fn selected_kind(&self) -> PatternKind {
        match self.kind_index {
            0 => PatternKind::OrchestratorWorker,
            1 => PatternKind::Hierarchical,
            2 => PatternKind::Blackboard,
            _ => PatternKind::Market,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicatePatternState {
    pub source_name: String,
    pub name: String,
    pub is_global: bool,
    pub active_field: usize, // 0: name, 1: target
}

// ============================================================================
// Section State
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdPatternsSectionState {
    pub selected: usize,
    pub filter_index: usize, // 0: Todos, 1: Proyecto, 2: Global, 3: Plantillas
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub patterns: Vec<PatternSummary>,
    pub current_detail: Option<PatternDetail>,
    pub sub_action: Option<PatternSubAction>,
    pub last_action: Option<ShepherdPatternsAction>,
    pub error: Option<String>,
}

impl Default for ShepherdPatternsSectionState {
    fn default() -> Self {
        let mut s = Self {
            selected: 0,
            filter_index: 0,
            list_scroll: 0,
            detail_scroll: 0,
            patterns: Vec::new(),
            current_detail: None,
            sub_action: None,
            last_action: None,
            error: None,
        };
        s.load_builtin_defaults();
        s
    }
}

impl ShepherdPatternsSectionState {
    pub fn load_builtin_defaults(&mut self) {
        if self.patterns.is_empty() {
            self.patterns = vec![
                PatternSummary {
                    name: "orchestrator-worker".into(),
                    kind: PatternKind::OrchestratorWorker,
                    description: "Central orchestrator dispatches tasks to a worker pool over partitioned topics.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 4,
                    topics_count: 2,
                    path: None,
                },
                PatternSummary {
                    name: "hierarchical".into(),
                    kind: PatternKind::Hierarchical,
                    description: "Multi-tier command hierarchy: root director dispatches to managers, who coordinate specialists.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 7,
                    topics_count: 4,
                    path: None,
                },
                PatternSummary {
                    name: "blackboard".into(),
                    kind: PatternKind::Blackboard,
                    description: "Shared knowledge base where specialist agents read hypotheses and post partial discoveries.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 3,
                    topics_count: 1,
                    path: None,
                },
                PatternSummary {
                    name: "market".into(),
                    kind: PatternKind::Market,
                    description: "Competitive market: requesters post tasks to ask topic, solvers bid, market-maker matches.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 6,
                    topics_count: 3,
                    path: None,
                },
            ];
            self.update_current_detail_from_selection();
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        self.patterns
            .iter()
            .enumerate()
            .filter(|(_, p)| match self.filter_index {
                1 => p.source == PatternSource::Project,
                2 => p.source == PatternSource::Global,
                3 => p.source == PatternSource::Builtin,
                _ => true,
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_summary(&self) -> Option<&PatternSummary> {
        let indices = self.filtered_indices();
        indices.get(self.selected).and_then(|&idx| self.patterns.get(idx))
    }

    pub fn update_from_summaries(&mut self, list: Vec<PatternSummary>) {
        self.patterns = list;
        let count = self.filtered_indices().len();
        if self.selected >= count && count > 0 {
            self.selected = count - 1;
        }
        self.update_current_detail_from_selection();
    }

    pub fn update_current_detail_from_selection(&mut self) {
        if let Some(sum) = self.selected_summary() {
            if let Ok((pat, src, path)) = shepherd_core::patterns::load_pattern(
                &sum.name,
                &std::path::PathBuf::from("."),
                None,
            ) {
                let config = shepherd_core::config::Config::default();
                let problems = validate(&pat, &config);
                self.current_detail = Some(PatternDetail {
                    pattern: pat,
                    source: src,
                    path: path.map(|p| p.display().to_string()),
                    problems,
                });
            } else if let Some(pat) = get_builtin_pattern(&sum.name) {
                let config = shepherd_core::config::Config::default();
                let problems = validate(&pat, &config);
                self.current_detail = Some(PatternDetail {
                    pattern: pat,
                    source: sum.source,
                    path: sum.path.clone(),
                    problems,
                });
            }
        }
    }

    pub fn update_detail(&mut self, detail: PatternDetail) {
        self.current_detail = Some(detail);
    }

    pub fn has_sub_action(&self) -> bool {
        self.sub_action.is_some()
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.sub_action.is_some() {
            self.sub_action = None;
            self.error = None;
            true
        } else {
            false
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.detail_scroll = 0;
            if self.selected < self.list_scroll {
                self.list_scroll = self.selected;
            }
            self.update_current_detail_from_selection();
        }
    }

    pub fn select_next(&mut self) {
        let count = self.filtered_indices().len();
        if count > 0 && self.selected + 1 < count {
            self.selected += 1;
            self.detail_scroll = 0;
            self.update_current_detail_from_selection();
        }
    }

    pub fn start_new_from_template(&mut self) {
        self.sub_action = Some(PatternSubAction::NewFromTemplate(NewFromTemplateState::default()));
        self.error = None;
    }

    pub fn start_duplicate(&mut self) {
        if let Some(sum) = self.selected_summary() {
            self.sub_action = Some(PatternSubAction::Duplicate(DuplicatePatternState {
                source_name: sum.name.clone(),
                name: format!("{}-copia", sum.name),
                is_global: false,
                active_field: 0,
            }));
            self.error = None;
        }
    }

    pub fn start_edit(&mut self) {
        if let Some(detail) = &self.current_detail {
            self.sub_action = Some(PatternSubAction::Edit(PatternEditorState::from_pattern(
                &detail.pattern,
                detail.source,
            )));
            self.error = None;
        } else if let Some(sum) = self.selected_summary() {
            if let Some(pat) = get_builtin_pattern(&sum.name) {
                self.sub_action = Some(PatternSubAction::Edit(PatternEditorState::from_pattern(
                    &pat,
                    sum.source,
                )));
                self.error = None;
            }
        }
    }

    pub fn start_delete(&mut self) {
        if let Some(sum) = self.selected_summary() {
            if sum.source == PatternSource::Builtin {
                self.error = Some("Las plantillas integradas del sistema son protegidas y no se pueden eliminar.".into());
                return;
            }
            self.sub_action = Some(PatternSubAction::ConfirmDelete {
                name: sum.name.clone(),
                source: sum.source,
            });
            self.error = None;
        }
    }

    pub fn validate_selected(&mut self) -> Option<ShepherdPatternsAction> {
        if let Some(sum) = self.selected_summary() {
            Some(ShepherdPatternsAction::Validate {
                name: Some(sum.name.clone()),
                pattern: None,
            })
        } else {
            None
        }
    }

    pub fn start_run(&mut self) {
        if let Some(sum) = self.selected_summary() {
            self.sub_action = Some(PatternSubAction::Run(PatternRunFormState::new(sum.name.clone())));
            self.error = None;
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdPatternsAction>) {
        // 1. If Sub-action is active
        if let Some(ref mut sub) = self.sub_action {
            match sub {
                PatternSubAction::ConfirmDelete { name, .. } => {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('c') => {
                            self.sub_action = None;
                            return (true, None);
                        }
                        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('d') => {
                            let act = ShepherdPatternsAction::Delete { name: name.clone() };
                            self.sub_action = None;
                            return (true, Some(act));
                        }
                        _ => return (false, None),
                    }
                }
                PatternSubAction::NewFromTemplate(state) => {
                    match key.code {
                        KeyCode::Esc => {
                            self.sub_action = None;
                            return (true, None);
                        }
                        KeyCode::Tab => {
                            state.active_field = (state.active_field + 1) % 3;
                            return (true, None);
                        }
                        KeyCode::BackTab => {
                            state.active_field = if state.active_field == 0 { 2 } else { state.active_field - 1 };
                            return (true, None);
                        }
                        KeyCode::Char(' ') => {
                            match state.active_field {
                                0 => {
                                    state.kind_index = (state.kind_index + 1) % 4;
                                    let kind_name = match state.kind_index {
                                        0 => "mi-orquestador",
                                        1 => "mi-jerarquico",
                                        2 => "mi-pizarra",
                                        _ => "mi-mercado",
                                    };
                                    state.name = kind_name.into();
                                }
                                2 => state.is_global = !state.is_global,
                                _ => state.name.push(' '),
                            }
                            return (true, None);
                        }
                        KeyCode::Backspace => {
                            if state.active_field == 1 {
                                state.name.pop();
                            }
                            return (true, None);
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) => {
                            if state.active_field == 1 {
                                state.name.push(c);
                            }
                            return (true, None);
                        }
                        KeyCode::Enter => {
                            let kind = state.selected_kind();
                            let name = state.name.trim().to_string();
                            let is_global = state.is_global;
                            if !name.is_empty() {
                                let editor = PatternEditorState::new_empty(kind, &name, is_global);
                                self.sub_action = Some(PatternSubAction::Edit(editor));
                            }
                            return (true, None);
                        }
                        _ => return (false, None),
                    }
                }
                PatternSubAction::Duplicate(state) => {
                    match key.code {
                        KeyCode::Esc => {
                            self.sub_action = None;
                            return (true, None);
                        }
                        KeyCode::Tab => {
                            state.active_field = (state.active_field + 1) % 2;
                            return (true, None);
                        }
                        KeyCode::BackTab => {
                            state.active_field = if state.active_field == 0 { 1 } else { 0 };
                            return (true, None);
                        }
                        KeyCode::Char(' ') if state.active_field == 1 => {
                            state.is_global = !state.is_global;
                            return (true, None);
                        }
                        KeyCode::Backspace => {
                            if state.active_field == 0 {
                                state.name.pop();
                            }
                            return (true, None);
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) => {
                            if state.active_field == 0 {
                                state.name.push(c);
                            }
                            return (true, None);
                        }
                        KeyCode::Enter => {
                            let src_name = state.source_name.clone();
                            let target_name = state.name.trim().to_string();
                            let is_global = state.is_global;
                            if !target_name.is_empty() {
                                let base_pat = get_builtin_pattern(&src_name).or_else(|| {
                                    self.current_detail.as_ref().map(|d| d.pattern.clone())
                                });
                                if let Some(mut pat) = base_pat {
                                    pat.name = target_name;
                                    let mut editor = PatternEditorState::from_pattern(
                                        &pat,
                                        if is_global { PatternSource::Global } else { PatternSource::Project },
                                    );
                                    editor.is_global = is_global;
                                    editor.is_new = true;
                                    self.sub_action = Some(PatternSubAction::Edit(editor));
                                }
                            }
                            return (true, None);
                        }
                        _ => return (false, None),
                    }
                }
                PatternSubAction::Edit(editor) => {
                    match key.code {
                        KeyCode::Esc => {
                            self.sub_action = None;
                            return (true, None);
                        }
                        KeyCode::Tab => {
                            editor.cycle_forward();
                            return (true, None);
                        }
                        KeyCode::BackTab => {
                            editor.cycle_backward();
                            return (true, None);
                        }
                        // Section switches with 1-4 when not typing in text fields
                        KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.active_section = EditorSection::Metadata;
                            editor.active_field = 0;
                            return (true, None);
                        }
                        KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.active_section = EditorSection::Agents;
                            editor.active_field = 0;
                            return (true, None);
                        }
                        KeyCode::Char('3') if key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.active_section = EditorSection::Topics;
                            editor.active_field = 0;
                            return (true, None);
                        }
                        KeyCode::Char('4') if key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.active_section = EditorSection::Topology;
                            return (true, None);
                        }
                        // Space toggles choices in metadata
                        KeyCode::Char(' ') if editor.active_section == EditorSection::Metadata && (editor.active_field == 1 || editor.active_field == 2) => {
                            editor.toggle_active_choice();
                            return (true, None);
                        }
                        // Agent manipulation shortcuts
                        KeyCode::Char('[') if key.modifiers.contains(KeyModifiers::ALT) => {
                            if editor.active_section == EditorSection::Agents && editor.selected_agent > 0 {
                                editor.selected_agent -= 1;
                                editor.active_field = 0;
                                return (true, None);
                            }
                            if editor.active_section == EditorSection::Topics && editor.selected_topic > 0 {
                                editor.selected_topic -= 1;
                                editor.active_field = 0;
                                return (true, None);
                            }
                        }
                        KeyCode::Char(']') if key.modifiers.contains(KeyModifiers::ALT) => {
                            if editor.active_section == EditorSection::Agents && editor.selected_agent + 1 < editor.agents.len() {
                                editor.selected_agent += 1;
                                editor.active_field = 0;
                                return (true, None);
                            }
                            if editor.active_section == EditorSection::Topics && editor.selected_topic + 1 < editor.topics.len() {
                                editor.selected_topic += 1;
                                editor.active_field = 0;
                                return (true, None);
                            }
                        }
                        KeyCode::Char('+') | KeyCode::Char('A') if editor.active_section == EditorSection::Agents && key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.add_agent();
                            return (true, None);
                        }
                        KeyCode::Char('-') | KeyCode::Char('X') if editor.active_section == EditorSection::Agents && key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.remove_agent();
                            return (true, None);
                        }
                        KeyCode::Char('K') if editor.active_section == EditorSection::Agents && key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.move_agent_up();
                            return (true, None);
                        }
                        KeyCode::Char('J') if editor.active_section == EditorSection::Agents && key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.move_agent_down();
                            return (true, None);
                        }
                        // Topic manipulation shortcuts
                        KeyCode::Char('+') | KeyCode::Char('T') if editor.active_section == EditorSection::Topics && key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.add_topic();
                            return (true, None);
                        }
                        KeyCode::Char('-') | KeyCode::Char('D') if editor.active_section == EditorSection::Topics && key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.remove_topic();
                            return (true, None);
                        }
                        // Text editing
                        KeyCode::Backspace => {
                            editor.delete_char();
                            return (true, None);
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.insert_char(c);
                            return (true, None);
                        }
                        // Save pattern with Enter or Ctrl+S
                        KeyCode::Enter | KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) || (key.code == KeyCode::Enter && editor.active_field == editor.field_count_for_section().saturating_sub(1)) => {
                            if editor.is_valid() {
                                let pat = editor.to_pattern();
                                let is_global = editor.is_global;
                                let act = ShepherdPatternsAction::Save { pattern: pat, global: is_global };
                                self.sub_action = None;
                                return (true, Some(act));
                            } else {
                                self.error = Some("No se puede guardar: corrija los errores de validación.".into());
                                return (true, None);
                            }
                        }
                        _ => return (false, None),
                    }
                }
                PatternSubAction::Run(state) => {
                    match key.code {
                        KeyCode::Esc => {
                            self.sub_action = None;
                            return (true, None);
                        }
                        KeyCode::Tab => {
                            state.active_field = (state.active_field + 1) % 2;
                            return (true, None);
                        }
                        KeyCode::BackTab => {
                            state.active_field = if state.active_field == 0 { 1 } else { 0 };
                            return (true, None);
                        }
                        KeyCode::Backspace => {
                            if state.active_field == 0 {
                                state.task.pop();
                            } else {
                                state.worktree.pop();
                            }
                            return (true, None);
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) => {
                            if state.active_field == 0 {
                                state.task.push(c);
                            } else {
                                state.worktree.push(c);
                            }
                            return (true, None);
                        }
                        KeyCode::Enter => {
                            let pattern = state.pattern_name.clone();
                            let task = if state.task.trim().is_empty() { None } else { Some(state.task.trim().to_string()) };
                            let worktree = if state.worktree.trim().is_empty() { None } else { Some(state.worktree.trim().to_string()) };
                            let act = ShepherdPatternsAction::RunPattern {
                                pattern,
                                task,
                                worktree,
                            };
                            self.sub_action = None;
                            return (true, Some(act));
                        }
                        _ => return (false, None),
                    }
                }
            }
        }

        // 2. Normal List / Gallery Navigation
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                let act = self.selected_summary().map(|s| ShepherdPatternsAction::Get { name: s.name.clone() });
                (true, act)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                let act = self.selected_summary().map(|s| ShepherdPatternsAction::Get { name: s.name.clone() });
                (true, act)
            }
            KeyCode::Char('n') => {
                self.start_new_from_template();
                (true, None)
            }
            KeyCode::Char('c') => {
                self.start_duplicate();
                (true, None)
            }
            KeyCode::Char('e') => {
                self.start_edit();
                (true, None)
            }
            KeyCode::Char('r') => {
                self.start_run();
                (true, None)
            }
            KeyCode::Char('d') => {
                self.start_delete();
                (true, None)
            }
            KeyCode::Char('v') => {
                let act = self.validate_selected();
                (true, act)
            }
            KeyCode::Char('f') => {
                self.filter_index = (self.filter_index + 1) % 4;
                self.selected = 0;
                self.update_current_detail_from_selection();
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

    pub fn handle_mouse(&mut self, col: u16, row: u16, hits: &ShepherdPatternsHits) -> (bool, Option<ShepherdPatternsAction>) {
        // 1. Stripe hits
        if let Some(ref sh) = hits.stripe_hits {
            if let Some(c_rect) = sh.confirm_button {
                if c_rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::ConfirmDelete { ref name, .. }) = self.sub_action {
                        let act = ShepherdPatternsAction::Delete { name: name.clone() };
                        self.sub_action = None;
                        return (true, Some(act));
                    }
                }
            }
            if let Some(cancel_rect) = sh.cancel_button {
                if cancel_rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        // 2. Editor hits
        if let Some(ref eh) = hits.editor_hits {
            for (rect, idx) in &eh.tabs {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.active_section = EditorSection::from_index(*idx);
                        ed.active_field = 0;
                        return (true, None);
                    }
                }
            }
            for (rect, idx) in &eh.field_rects {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.active_field = *idx;
                        return (true, None);
                    }
                }
            }
            for (rect, idx) in &eh.agent_tabs {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.selected_agent = *idx;
                        return (true, None);
                    }
                }
            }
            for (rect, idx) in &eh.topic_tabs {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.selected_topic = *idx;
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.add_agent_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.add_agent();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.remove_agent_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.remove_agent();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.move_agent_up_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.move_agent_up();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.move_agent_down_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.move_agent_down();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.add_topic_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.add_topic();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.remove_topic_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.remove_topic();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref ed)) = self.sub_action {
                        if ed.is_valid() {
                            let act = ShepherdPatternsAction::Save {
                                pattern: ed.to_pattern(),
                                global: ed.is_global,
                            };
                            self.sub_action = None;
                            return (true, Some(act));
                        }
                    }
                }
            }
            if let Some(r) = eh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        // 3. New from template form hits
        if let Some(ref nfh) = hits.new_from_template_hits {
            if let Some(r) = nfh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::NewFromTemplate(ref st)) = self.sub_action {
                        let kind = st.selected_kind();
                        let name = st.name.trim().to_string();
                        let is_global = st.is_global;
                        if !name.is_empty() {
                            let editor = PatternEditorState::new_empty(kind, &name, is_global);
                            self.sub_action = Some(PatternSubAction::Edit(editor));
                            return (true, None);
                        }
                    }
                }
            }
            if let Some(r) = nfh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        // 4. Duplicate form hits
        if let Some(ref dfh) = hits.duplicate_hits {
            if let Some(r) = dfh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Duplicate(ref st)) = self.sub_action {
                        let src_name = st.source_name.clone();
                        let target_name = st.name.trim().to_string();
                        let is_global = st.is_global;
                        if !target_name.is_empty() {
                            let base_pat = get_builtin_pattern(&src_name).or_else(|| {
                                self.current_detail.as_ref().map(|d| d.pattern.clone())
                            });
                            if let Some(mut pat) = base_pat {
                                pat.name = target_name;
                                let mut editor = PatternEditorState::from_pattern(
                                    &pat,
                                    if is_global { PatternSource::Global } else { PatternSource::Project },
                                );
                                editor.is_global = is_global;
                                editor.is_new = true;
                                self.sub_action = Some(PatternSubAction::Edit(editor));
                                return (true, None);
                            }
                        }
                    }
                }
            }
            if let Some(r) = dfh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        // 5. Run form hits
        if let Some(ref rfh) = hits.run_form_hits {
            if let Some(r) = rfh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Run(ref st)) = self.sub_action {
                        let pattern = st.pattern_name.clone();
                        let task = if st.task.trim().is_empty() { None } else { Some(st.task.trim().to_string()) };
                        let worktree = if st.worktree.trim().is_empty() { None } else { Some(st.worktree.trim().to_string()) };
                        let act = ShepherdPatternsAction::RunPattern {
                            pattern,
                            task,
                            worktree,
                        };
                        self.sub_action = None;
                        return (true, Some(act));
                    }
                }
            }
            if let Some(r) = rfh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
            for (f_rect, idx) in &rfh.field_rects {
                if f_rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Run(ref mut st)) = self.sub_action {
                        st.active_field = *idx;
                        return (true, None);
                    }
                }
            }
        }

        // 6. Empty state hits
        if let Some(ref eh) = hits.empty_hits {
            for (rect, idx) in &eh.actions {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if *idx == 0 {
                        self.start_new_from_template();
                        return (true, None);
                    }
                }
            }
        }

        // 6. List hits (rows, filter tabs, action button)
        for (rect, row_idx) in &hits.list_hits.rows {
            if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                self.selected = *row_idx;
                self.detail_scroll = 0;
                self.update_current_detail_from_selection();
                let act = self.selected_summary().map(|s| ShepherdPatternsAction::Get { name: s.name.clone() });
                return (true, act);
            }
        }
        for (rect, tab_idx) in &hits.list_hits.filter_tabs {
            if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                self.filter_index = *tab_idx;
                self.selected = 0;
                self.update_current_detail_from_selection();
                return (true, None);
            }
        }
        if let Some(btn) = hits.list_hits.action_button {
            if btn.contains(ratatui::layout::Position { x: col, y: row }) {
                self.start_new_from_template();
                return (true, None);
            }
        }

        // 7. Detail view actions
        if let Some(ref dh) = hits.detail_hits {
            for (rect, idx) in &dh.header_actions {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    match idx {
                        0 => { self.start_edit(); return (true, None); }
                        1 => { self.start_duplicate(); return (true, None); }
                        2 => { let act = self.validate_selected(); return (true, act); }
                        3 => { self.start_delete(); return (true, None); }
                        4 => { self.start_run(); return (true, None); }
                        _ => {}
                    }
                }
            }
            for (rect, idx) in &dh.bottom_actions {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    match idx {
                        0 => { self.start_edit(); return (true, None); }
                        1 => { self.start_duplicate(); return (true, None); }
                        2 => { let act = self.validate_selected(); return (true, act); }
                        3 => { self.start_delete(); return (true, None); }
                        4 => { self.start_run(); return (true, None); }
                        _ => {}
                    }
                }
            }
        }

        (false, None)
    }
}

// ============================================================================
// Pure Render Functions
// ============================================================================

pub fn render_patterns_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &ShepherdPatternsSectionState,
    palette: &Palette,
) -> ShepherdPatternsHits {
    let mut hits = ShepherdPatternsHits::default();
    if area.width < 20 || area.height < 5 {
        return hits;
    }

    // Split area: left list (~30-35%), right detail/editor (~65-70%)
    let list_width = if area.width >= 100 {
        (area.width * 30 / 100).max(26)
    } else if area.width >= 70 {
        (area.width * 34 / 100).max(24)
    } else {
        area.width
    };

    let list_area = Rect::new(area.x, area.y, list_width, area.height);

    // Prepare list items
    let filtered_indices = state.filtered_indices();
    let list_items: Vec<ListViewItem> = filtered_indices
        .iter()
        .map(|&idx| {
            let p = &state.patterns[idx];
            let badge_color = match p.source {
                PatternSource::Project => palette.green,
                PatternSource::Global => palette.yellow,
                PatternSource::Builtin => palette.overlay0,
            };
            let badge_text = format!("[{}]", p.source.as_str().to_uppercase());
            let meta = format!("{} ({} ag.)", p.kind.as_str(), p.agents_count);
            ListViewItem {
                title: p.name.clone(),
                badge: Some((badge_text, Style::default().fg(badge_color))),
                meta: Some(meta),
            }
        })
        .collect();

    let filter_tabs = ["Todos", "Proyecto", "Global", "Plantillas"];
    hits.list_hits = render_list_view(
        buffer,
        list_area,
        &list_items,
        state.selected,
        state.list_scroll,
        Some((&filter_tabs, state.filter_index)),
        Some(("+ Nuevo", "n")),
        palette,
    );

    // If terminal width >= 70, render right panel
    if area.width >= 70 {
        let sep_x = list_area.right();
        for y in area.y..area.bottom() {
            let cell = &mut buffer[(sep_x, y)];
            cell.set_char('│');
            cell.set_fg(palette.surface_dim);
        }

        let detail_x = sep_x + 1;
        let detail_w = area.right().saturating_sub(detail_x);
        let detail_area = Rect::new(detail_x, area.y, detail_w, area.height);

        // 1. If SubAction is active
        if let Some(ref sub) = state.sub_action {
            match sub {
                PatternSubAction::ConfirmDelete { name, source } => {
                    let warn = format!(
                        "¿Eliminar permanentemente el patrón '{}' de ámbito {}? Esta acción no se puede deshacer.",
                        name, source.as_str()
                    );
                    hits.stripe_hits = Some(render_confirm_stripe(
                        buffer,
                        detail_area,
                        &warn,
                        "Confirmar eliminación (d)",
                        "Cancelar (Esc)",
                        true,
                        palette,
                    ));
                    return hits;
                }
                PatternSubAction::NewFromTemplate(st) => {
                    let inline_form = InlineFormState {
                        title: "NUEVO PATRÓN DESDE PLANTILLA".into(),
                        fields: vec![
                            FormField {
                                label: format!("Plantilla base (Espacio para cambiar): [{}]", st.selected_kind().as_str()),
                                value: st.selected_kind().as_str().into(),
                                placeholder: "".into(),
                            },
                            FormField {
                                label: "Nombre del nuevo patrón:".into(),
                                value: st.name.clone(),
                                placeholder: "mi-patron".into(),
                            },
                            FormField {
                                label: format!("Ámbito de guardado (Espacio para alternar): [{}]", if st.is_global { "Global (~/.config)" } else { "Proyecto (.shepherd)" }),
                                value: if st.is_global { "Global" } else { "Proyecto" }.into(),
                                placeholder: "".into(),
                            },
                        ],
                        active_field: st.active_field,
                        error: None,
                        success: None,
                    };
                    hits.new_from_template_hits = Some(render_inline_form(
                        buffer,
                        detail_area,
                        &inline_form,
                        "Crear y Editar (Enter)",
                        "Cancelar (Esc)",
                        !st.name.trim().is_empty(),
                        palette,
                    ));
                    return hits;
                }
                PatternSubAction::Duplicate(st) => {
                    let inline_form = InlineFormState {
                        title: format!("DUPLICAR PATRÓN: {}", st.source_name),
                        fields: vec![
                            FormField {
                                label: "Nombre del nuevo patrón:".into(),
                                value: st.name.clone(),
                                placeholder: "nombre-copia".into(),
                            },
                            FormField {
                                label: format!("Ámbito de guardado (Espacio para alternar): [{}]", if st.is_global { "Global (~/.config)" } else { "Proyecto (.shepherd)" }),
                                value: if st.is_global { "Global" } else { "Proyecto" }.into(),
                                placeholder: "".into(),
                            },
                        ],
                        active_field: st.active_field,
                        error: None,
                        success: None,
                    };
                    hits.duplicate_hits = Some(render_inline_form(
                        buffer,
                        detail_area,
                        &inline_form,
                        "Duplicar y Editar (Enter)",
                        "Cancelar (Esc)",
                        !st.name.trim().is_empty(),
                        palette,
                    ));
                    return hits;
                }
                PatternSubAction::Edit(editor) => {
                    hits.editor_hits = Some(render_pattern_editor(
                        buffer,
                        detail_area,
                        editor,
                        palette,
                    ));
                    return hits;
                }
                PatternSubAction::Run(st) => {
                    let inline_form = InlineFormState {
                        title: format!("EJECUTAR PATRÓN: {}", st.pattern_name),
                        fields: vec![
                            FormField {
                                label: "Objetivo o tarea (opcional):".into(),
                                value: st.task.clone(),
                                placeholder: "p.ej. Implementar endpoint de métricas".into(),
                            },
                            FormField {
                                label: "Worktree aislado (opcional):".into(),
                                value: st.worktree.clone(),
                                placeholder: "p.ej. /tmp/worktree-f8d".into(),
                            },
                        ],
                        active_field: st.active_field,
                        error: None,
                        success: None,
                    };
                    hits.run_form_hits = Some(render_inline_form(
                        buffer,
                        detail_area,
                        &inline_form,
                        "Iniciar Ejecución (Enter)",
                        "Cancelar (Esc)",
                        true,
                        palette,
                    ));
                    return hits;
                }
            }
        }

        // 2. Normal Detail View
        if let Some(detail) = &state.current_detail {
            let error_count = detail.problems.iter().filter(|p| p.is_error()).count();
            let _warn_count = detail.problems.iter().filter(|p| !p.is_error()).count();
            let validity_badge = if error_count == 0 {
                (format!("[✓ Válido]"), Style::default().fg(palette.green))
            } else {
                (format!("[✗ {} errores]", error_count), Style::default().fg(palette.red))
            };

            let src_badge = (
                format!("[{}]", detail.source.as_str().to_uppercase()),
                Style::default().fg(match detail.source {
                    PatternSource::Project => palette.green,
                    PatternSource::Global => palette.yellow,
                    PatternSource::Builtin => palette.overlay0,
                }),
            );

            let kind_badge = (
                format!("[{}]", detail.pattern.kind.as_str()),
                Style::default().fg(palette.accent),
            );

            let meta = format!(
                "Topología: {} | Agentes: {} | Tópicos: {}{}",
                detail.pattern.kind.as_str(),
                detail.pattern.agents.len(),
                detail.pattern.topics.len(),
                detail.path.as_deref().map(|p| format!(" | {}", p)).unwrap_or_default()
            );

            let is_valid = error_count == 0;
            let header = DetailHeader {
                title: format!("PATRÓN: {}", detail.pattern.name),
                badges: vec![src_badge, kind_badge, validity_badge],
                metadata_line: Some(meta),
                actions: vec![
                    ("Editar".into(), Some("e".into())),
                    ("Duplicar".into(), Some("c".into())),
                    ("Validar".into(), Some("v".into())),
                    ("Eliminar".into(), Some("d".into())),
                    ("Ejecutar".into(), Some("r".into())),
                ],
            };

            let mut blocks = Vec::new();

            // Description
            blocks.push(DetailContentBlock::Heading("DESCRIPCIÓN".into()));
            blocks.push(DetailContentBlock::Paragraph(detail.pattern.description.clone()));

            // Topology diagram
            blocks.push(DetailContentBlock::Heading("DIAGRAMA DE TOPOLOGÍA (ASCII)".into()));
            let diag_lines = generate_topology_diagram(&detail.pattern, detail_w.saturating_sub(4) as usize);
            blocks.push(DetailContentBlock::BulletList(diag_lines));

            // Agents summary
            blocks.push(DetailContentBlock::Heading(format!("AGENTES ({})", detail.pattern.agents.len())));
            let mut agent_entries = Vec::new();
            for a in &detail.pattern.agents {
                let mut desc = format!("• ID: {} | Rol: {}", a.id, a.role);
                if a.count > 1 {
                    desc.push_str(&format!(" (x{})", a.count));
                }
                if let Some(ref v) = a.vendor {
                    desc.push_str(&format!(" | vendor: {}", v));
                }
                if let Some(ref m) = a.model {
                    desc.push_str(&format!(" | model: {}", m));
                }
                if let Some(l) = a.level {
                    desc.push_str(&format!(" | nivel: {}", l));
                }
                if let Some(ref g) = a.group {
                    desc.push_str(&format!(" | grupo: {}", g));
                }
                if !a.publishes.is_empty() {
                    desc.push_str(&format!(" | publica: [{}]", a.publishes.join(", ")));
                }
                if !a.consumes.is_empty() {
                    desc.push_str(&format!(" | consume: [{}]", a.consumes.join(", ")));
                }
                if let Some(ref p) = a.prompt {
                    let prompt_snippet = truncate_line(p, 40);
                    desc.push_str(&format!(" | persona: \"{}\"", prompt_snippet));
                }
                agent_entries.push(desc);
            }
            blocks.push(DetailContentBlock::BulletList(agent_entries));

            // Topics summary
            blocks.push(DetailContentBlock::Heading(format!("TÓPICOS ({})", detail.pattern.topics.len())));
            let mut topic_entries = Vec::new();
            for t in &detail.pattern.topics {
                let mut desc = format!("• {}: {} partición(es)", t.name, t.partitions);
                if let Some(ref k) = t.key {
                    desc.push_str(&format!(" | clave: {}", k));
                }
                if let Some(ref f) = t.filter {
                    desc.push_str(&format!(" | filtro: {}", f));
                }
                topic_entries.push(desc);
            }
            blocks.push(DetailContentBlock::BulletList(topic_entries));

            // Validation problems block if any
            if !detail.problems.is_empty() {
                blocks.push(DetailContentBlock::Heading("DIAGNÓSTICOS DE VALIDACIÓN".into()));
                for p in &detail.problems {
                    blocks.push(DetailContentBlock::Notice {
                        text: format!("[{}] {}: {}", p.severity, p.path, p.message),
                        level: if p.is_error() { AlertLevel::Warning } else { AlertLevel::Info },
                    });
                }
            }

            let bottom_actions = [
                ("Editar", "e", true),
                ("Duplicar", "c", false),
                ("Validar", "v", false),
                ("Eliminar", "d", false),
                ("Ejecutar", "r", is_valid),
            ];

            hits.detail_hits = Some(render_detail_view(
                buffer,
                detail_area,
                &header,
                &blocks,
                state.detail_scroll,
                &bottom_actions,
                palette,
            ));
        } else if list_items.is_empty() {
            hits.empty_hits = Some(render_empty_state(
                buffer,
                detail_area,
                "SIN PATRONES",
                "No hay patrones disponibles en este filtro.\nCrea tu primer patrón multi-agente a partir de una plantilla.",
                &[("+ Nuevo desde Plantilla", Some("n"))],
                palette,
            ));
        }
    }

    hits
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
    let title = format!("EDITOR DE PATRÓN: {}", editor.name);
    put_str(
        buffer,
        area.x,
        current_y,
        area.width,
        &title,
        Style::default().fg(palette.accent).add_modifier(Modifier::BOLD),
    );
    current_y += 1;

    // 2. Section navigation tabs
    let tabs = ["1. Metadatos", "2. Agentes", "3. Tópicos", "4. Topología"];
    let mut tab_x = area.x;
    for (i, t_label) in tabs.iter().enumerate() {
        let is_active = editor.active_section.index() == i;
        let style = if is_active {
            Style::default().fg(palette.panel_bg).bg(palette.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0).bg(palette.surface0)
        };
        let formatted = format!(" [ {} ] ", t_label);
        let len = formatted.chars().count() as u16;
        if tab_x + len < area.right() {
            put_str(buffer, tab_x, current_y, len, &formatted, style);
            hits.tabs.push((Rect::new(tab_x, current_y, len, 1), i));
            tab_x += len + 1;
        }
    }
    current_y += 2;

    // Divider
    let sep = "─".repeat(area.width as usize);
    put_str(buffer, area.x, current_y - 1, area.width, &sep, Style::default().fg(palette.surface_dim));

    // 3. Content per active section
    match editor.active_section {
        EditorSection::Metadata => {
            // Fields: 0: Name, 1: Kind, 2: Target, 3: Description
            let fields_meta = [
                ("Nombre del patrón:", editor.name.as_str()),
                ("Tipo de topología (Espacio cambia):", editor.kind.as_str()),
                ("Ámbito de guardado (Espacio cambia):", if editor.is_global { "Global (~/.config)" } else { "Proyecto (.shepherd)" }),
                ("Descripción:", editor.description.as_str()),
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
                    Style::default().fg(palette.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0)
                };

                let mut full_label = label.to_string();
                if let Some(p) = prob {
                    full_label.push_str(&format!("  [✗ {}]", p.message));
                }
                put_str(buffer, area.x, current_y, area.width, &full_label, label_style);
                current_y += 1;

                let box_w = area.width.saturating_sub(2);
                let box_rect = Rect::new(area.x, current_y, box_w, 1);
                hits.field_rects.push((box_rect, i));

                let box_bg = if is_active { palette.surface0 } else { palette.panel_bg };
                let val_style = Style::default().fg(palette.accent).bg(box_bg);

                let display_val = if is_active {
                    format!("{}█", val)
                } else if val.is_empty() {
                    "(vacío)".into()
                } else {
                    val.to_string()
                };
                put_str(buffer, area.x, current_y, box_w, &display_val, val_style);
                current_y += 2;
            }
        }
        EditorSection::Agents => {
            // Sub-bar with agent selection and buttons
            let mut ag_x = area.x;
            for (i, ag) in editor.agents.iter().enumerate() {
                let is_sel = i == editor.selected_agent;
                let st = if is_sel {
                    Style::default().fg(palette.panel_bg).bg(palette.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0).bg(palette.surface0)
                };
                let txt = format!(" [{}: {}] ", ag.id, ag.role);
                let len = txt.chars().count() as u16;
                if ag_x + len < area.right().saturating_sub(25) {
                    put_str(buffer, ag_x, current_y, len, &txt, st);
                    hits.agent_tabs.push((Rect::new(ag_x, current_y, len, 1), i));
                    ag_x += len + 1;
                }
            }

            // Action buttons: [+ Añadir] [- Quitar] [↑] [↓]
            let add_rect = Rect::new(area.right().saturating_sub(24), current_y, 5, 1);
            put_str(buffer, add_rect.x, add_rect.y, 5, "[ + ]", Style::default().fg(palette.green));
            hits.add_agent_button = Some(add_rect);

            let rem_rect = Rect::new(area.right().saturating_sub(18), current_y, 5, 1);
            put_str(buffer, rem_rect.x, rem_rect.y, 5, "[ - ]", Style::default().fg(palette.red));
            hits.remove_agent_button = Some(rem_rect);

            let up_rect = Rect::new(area.right().saturating_sub(12), current_y, 5, 1);
            put_str(buffer, up_rect.x, up_rect.y, 5, "[ ↑ ]", Style::default().fg(palette.accent));
            hits.move_agent_up_button = Some(up_rect);

            let dn_rect = Rect::new(area.right().saturating_sub(6), current_y, 5, 1);
            put_str(buffer, dn_rect.x, dn_rect.y, 5, "[ ↓ ]", Style::default().fg(palette.accent));
            hits.move_agent_down_button = Some(dn_rect);

            current_y += 2;

            if let Some(agent) = editor.agents.get(editor.selected_agent) {
                let count_str = agent.count.to_string();
                let lvl_str = agent.level.map(|l| l.to_string()).unwrap_or_default();
                let pub_str = agent.publishes.join(", ");
                let sub_str = agent.consumes.join(", ");

                let fields_agent = [
                    ("ID del agente:", agent.id.as_str()),
                    ("Rol (brain, coder, reviewer, tester, docs):", agent.role.as_str()),
                    ("Vendor (opcional: claude, codex, gemini):", agent.vendor.as_deref().unwrap_or("")),
                    ("Modelo (opcional):", agent.model.as_deref().unwrap_or("")),
                    ("Instancias (count):", count_str.as_str()),
                    ("Nivel jerárquico (level, solo jerárquico):", lvl_str.as_str()),
                    ("Grupo de consumidores (group):", agent.group.as_deref().unwrap_or("")),
                    ("Tópicos que publica (coma):", pub_str.as_str()),
                    ("Tópicos a los que se suscribe (coma):", sub_str.as_str()),
                    ("Prompt / Persona (personalidad en lenguaje natural):", agent.prompt.as_deref().unwrap_or("")),
                ];

                for (i, (label, val)) in fields_agent.iter().enumerate() {
                    if current_y + 1 >= area.bottom().saturating_sub(3) {
                        break;
                    }
                    let is_active = i == editor.active_field;
                    let path = format!("agents[{}].{}", editor.selected_agent, match i {
                        0 => "id",
                        1 => "role",
                        2 => "vendor",
                        3 => "model",
                        4 => "count",
                        5 => "level",
                        6 => "group",
                        7 => "publishes",
                        8 => "consumes",
                        _ => "prompt",
                    });
                    let prob = editor.field_problem(&path);

                    let label_style = if is_active {
                        Style::default().fg(palette.accent).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette.overlay0)
                    };

                    let mut full_label = label.to_string();
                    if let Some(p) = prob {
                        full_label.push_str(&format!("  [✗ {}]", p.message));
                    }
                    put_str(buffer, area.x, current_y, area.width, &full_label, label_style);
                    current_y += 1;

                    let box_w = area.width.saturating_sub(2);
                    let box_rect = Rect::new(area.x, current_y, box_w, 1);
                    hits.field_rects.push((box_rect, i));

                    let box_bg = if is_active { palette.surface0 } else { palette.panel_bg };
                    let val_style = Style::default().fg(palette.accent).bg(box_bg);

                    let display_val = if is_active {
                        format!("{}█", val)
                    } else if val.is_empty() {
                        "(vacío / por defecto)".into()
                    } else {
                        val.to_string()
                    };
                    put_str(buffer, area.x, current_y, box_w, &display_val, val_style);
                    current_y += 1;
                }
            }
        }
        EditorSection::Topics => {
            // Topics list bar
            let mut top_x = area.x;
            for (i, top) in editor.topics.iter().enumerate() {
                let is_sel = i == editor.selected_topic;
                let st = if is_sel {
                    Style::default().fg(palette.panel_bg).bg(palette.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0).bg(palette.surface0)
                };
                let txt = format!(" [{}: (p:{})] ", top.name, top.partitions);
                let len = txt.chars().count() as u16;
                if top_x + len < area.right().saturating_sub(14) {
                    put_str(buffer, top_x, current_y, len, &txt, st);
                    hits.topic_tabs.push((Rect::new(top_x, current_y, len, 1), i));
                    top_x += len + 1;
                }
            }

            let add_rect = Rect::new(area.right().saturating_sub(13), current_y, 5, 1);
            put_str(buffer, add_rect.x, add_rect.y, 5, "[ + ]", Style::default().fg(palette.green));
            hits.add_topic_button = Some(add_rect);

            let rem_rect = Rect::new(area.right().saturating_sub(7), current_y, 5, 1);
            put_str(buffer, rem_rect.x, rem_rect.y, 5, "[ - ]", Style::default().fg(palette.red));
            hits.remove_topic_button = Some(rem_rect);

            current_y += 2;

            if let Some(topic) = editor.topics.get(editor.selected_topic) {
                let p_str = topic.partitions.to_string();
                let fields_topic = [
                    ("Nombre del tópico:", topic.name.as_str()),
                    ("Número de particiones (1..256):", p_str.as_str()),
                    ("Campo clave de partición (opcional):", topic.key.as_deref().unwrap_or("")),
                    ("Filtro de tópico (opcional):", topic.filter.as_deref().unwrap_or("")),
                ];

                for (i, (label, val)) in fields_topic.iter().enumerate() {
                    if current_y + 2 >= area.bottom().saturating_sub(3) {
                        break;
                    }
                    let is_active = i == editor.active_field;
                    let path = format!("topics[{}].{}", editor.selected_topic, match i {
                        0 => "name",
                        1 => "partitions",
                        2 => "key",
                        _ => "filter",
                    });
                    let prob = editor.field_problem(&path);

                    let label_style = if is_active {
                        Style::default().fg(palette.accent).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette.overlay0)
                    };

                    let mut full_label = label.to_string();
                    if let Some(p) = prob {
                        full_label.push_str(&format!("  [✗ {}]", p.message));
                    }
                    put_str(buffer, area.x, current_y, area.width, &full_label, label_style);
                    current_y += 1;

                    let box_w = area.width.saturating_sub(2);
                    let box_rect = Rect::new(area.x, current_y, box_w, 1);
                    hits.field_rects.push((box_rect, i));

                    let box_bg = if is_active { palette.surface0 } else { palette.panel_bg };
                    let val_style = Style::default().fg(palette.accent).bg(box_bg);

                    let display_val = if is_active {
                        format!("{}█", val)
                    } else if val.is_empty() {
                        "(vacío)".into()
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
                put_str(buffer, area.x, current_y, area.width, &line, Style::default().fg(palette.accent));
                current_y += 1;
            }
        }
    }

    // 4. Live validation feedback banner & bottom action buttons
    let bottom_y = area.bottom().saturating_sub(2);
    let error_count = editor.problems.iter().filter(|p| p.is_error()).count();
    let warn_count = editor.problems.iter().filter(|p| !p.is_error()).count();

    if error_count == 0 && warn_count == 0 {
        let ok_msg = "✓ Patrón válido (0 errores, 0 avisos) - listo para guardar";
        put_str(buffer, area.x, bottom_y, area.width, ok_msg, Style::default().fg(palette.green));
    } else if error_count > 0 {
        let first_err = editor.problems.iter().find(|p| p.is_error()).map(|p| p.message.as_str()).unwrap_or("");
        let err_msg = format!("✗ Hay {} error(es): {}", error_count, first_err);
        put_str(buffer, area.x, bottom_y, area.width, &err_msg, Style::default().fg(palette.red).add_modifier(Modifier::BOLD));
    } else {
        let first_warn = editor.problems.iter().find(|p| !p.is_error()).map(|p| p.message.as_str()).unwrap_or("");
        let warn_msg = format!("⚠ {} aviso(s): {}", warn_count, first_warn);
        put_str(buffer, area.x, bottom_y, area.width, &warn_msg, Style::default().fg(palette.yellow));
    }

    // Buttons
    let btn_y = area.bottom().saturating_sub(1);
    let submit_label = if editor.is_valid() {
        "[ Guardar (Enter) ]"
    } else {
        "[ Guardar bloqueado ]"
    };
    let submit_len = submit_label.chars().count() as u16;
    let submit_rect = Rect::new(area.x, btn_y, submit_len, 1);
    let submit_style = if editor.is_valid() {
        Style::default().fg(palette.panel_bg).bg(palette.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.red).bg(palette.surface0)
    };
    put_str(buffer, submit_rect.x, submit_rect.y, submit_len, submit_label, submit_style);
    hits.submit_button = Some(submit_rect);

    let cancel_label = "[ Cancelar (Esc) ]";
    let cancel_len = cancel_label.chars().count() as u16;
    let cancel_rect = Rect::new(submit_rect.right() + 2, btn_y, cancel_len, 1);
    put_str(buffer, cancel_rect.x, cancel_rect.y, cancel_len, cancel_label, Style::default().fg(palette.overlay0));
    hits.cancel_button = Some(cancel_rect);

    hits
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topology_diagram_orchestrator_worker() {
        let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
        let wide = generate_topology_diagram(&pat, 80);
        assert!(!wide.is_empty());
        assert!(wide.iter().any(|l| l.contains("[Orquestador]") || l.contains("brain")));
        assert!(wide.iter().any(|l| l.contains("Worker") || l.contains("coder")));
        assert!(wide.iter().any(|l| l.contains("orders")));

        let narrow = generate_topology_diagram(&pat, 40);
        assert!(!narrow.is_empty());
        for line in &narrow {
            assert!(line.chars().count() <= 40, "line exceeds narrow width: {}", line);
        }
    }

    #[test]
    fn test_topology_diagram_hierarchical() {
        let pat = get_builtin_pattern("hierarchical").expect("template exists");
        let wide = generate_topology_diagram(&pat, 80);
        assert!(wide.iter().any(|l| l.contains("Director") || l.contains("Nivel 0")));
        assert!(wide.iter().any(|l| l.contains("Managers") || l.contains("Nivel 1")));

        let narrow = generate_topology_diagram(&pat, 40);
        for line in &narrow {
            assert!(line.chars().count() <= 40);
        }
    }

    #[test]
    fn test_topology_diagram_blackboard() {
        let pat = get_builtin_pattern("blackboard").expect("template exists");
        let wide = generate_topology_diagram(&pat, 80);
        assert!(wide.iter().any(|l| l.contains("PIZARRA") || l.contains("blackboard")));

        let narrow = generate_topology_diagram(&pat, 40);
        for line in &narrow {
            assert!(line.chars().count() <= 40);
        }
    }

    #[test]
    fn test_topology_diagram_market() {
        let pat = get_builtin_pattern("market").expect("template exists");
        let wide = generate_topology_diagram(&pat, 80);
        assert!(wide.iter().any(|l| l.contains("Requesters") || l.contains("ask")));
        assert!(wide.iter().any(|l| l.contains("Market-Maker") || l.contains("bid")));

        let narrow = generate_topology_diagram(&pat, 40);
        for line in &narrow {
            assert!(line.chars().count() <= 40);
        }
    }

    #[test]
    fn test_patterns_section_pure_render_and_gallery() {
        let mut state = ShepherdPatternsSectionState::default();
        let palette = Palette::catppuccin();
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);

        let hits = render_patterns_section(&mut buffer, area, &state, &palette);
        assert!(!hits.list_hits.rows.is_empty(), "gallery should display patterns");
        assert!(hits.detail_hits.is_some(), "selected pattern detail should render");

        // Filter switching
        state.filter_index = 1; // Project
        assert_eq!(state.filtered_indices().len(), 0);
        state.filter_index = 3; // Builtin
        assert_eq!(state.filtered_indices().len(), 4);
    }

    #[test]
    fn test_pattern_editor_live_validation_and_guards() {
        let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
        let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);
        assert!(editor.is_valid(), "builtin pattern must start valid");

        // Provoke an error: empty name
        editor.name = String::new();
        editor.revalidate();
        assert!(!editor.is_valid(), "empty pattern name must be invalid");

        // Restore name
        editor.name = "mi-patron".into();
        editor.revalidate();
        assert!(editor.is_valid());

        // Provoke topology error: remove all workers
        editor.agents.retain(|a| a.role == "brain");
        editor.revalidate();
        assert!(!editor.is_valid(), "orchestrator without workers must be invalid");

        // Add back worker
        editor.add_agent();
        let w = editor.agents.last_mut().unwrap();
        w.role = "coder".into();
        w.group = Some("worker-pool".into());
        w.consumes = vec!["orders".into()];
        w.publishes = vec!["results".into()];
        editor.revalidate();
        assert!(editor.is_valid(), "valid topology restored");
    }

    #[test]
    fn test_patterns_section_state_machine_and_sub_actions() {
        let mut state = ShepherdPatternsSectionState::default();

        // 1. New from template
        state.start_new_from_template();
        assert!(state.has_sub_action());
        assert!(state.cancel_sub_action());
        assert!(!state.has_sub_action());

        // 2. Duplicate
        state.start_duplicate();
        assert!(state.has_sub_action());
        assert!(state.cancel_sub_action());

        // 3. Delete built-in is blocked
        state.start_delete();
        assert!(!state.has_sub_action(), "cannot delete builtin");
        assert!(state.error.is_some());

        // 4. Edit
        state.start_edit();
        assert!(state.has_sub_action());
        let (handled, _) = state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(handled);
        assert!(!state.has_sub_action(), "Esc cancels edit");
    }

    #[test]
    fn test_patterns_section_80x24_dimensions_no_panic() {
        let state = ShepherdPatternsSectionState::default();
        let palette = Palette::catppuccin();
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);

        let hits = render_patterns_section(&mut buffer, area, &state, &palette);
        assert!(!hits.list_hits.rows.is_empty());
    }

    #[test]
    fn test_pattern_editor_field_focus_and_navigation() {
        let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
        let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);

        // Section 0: Metadata
        assert_eq!(editor.active_section, EditorSection::Metadata);
        assert_eq!(editor.active_field, 0);
        editor.cycle_forward();
        assert_eq!(editor.active_field, 1);
        editor.toggle_active_choice(); // cycles kind
        assert_eq!(editor.kind, PatternKind::Hierarchical);
        editor.cycle_forward();
        assert_eq!(editor.active_field, 2);
        editor.toggle_active_choice(); // toggles global
        assert!(editor.is_global);
        editor.cycle_forward();
        assert_eq!(editor.active_field, 3);
        editor.cycle_forward();
        assert_eq!(editor.active_field, 0);
        editor.cycle_backward();
        assert_eq!(editor.active_field, 3);

        // Text typing in description
        editor.insert_char('!');
        assert!(editor.description.ends_with('!'));
        editor.delete_char();
        assert!(!editor.description.ends_with('!'));

        // Switch to Agents section
        editor.active_section = EditorSection::Agents;
        editor.active_field = 0;
        assert_eq!(editor.field_count_for_section(), 10);
        editor.cycle_forward();
        assert_eq!(editor.active_field, 1);
    }

    #[test]
    fn test_pattern_editor_agent_add_remove_and_reorder() {
        let pat = get_builtin_pattern("blackboard").expect("template exists");
        let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);
        let initial_count = editor.agents.len();
        assert_eq!(initial_count, 3);

        // Add agent
        editor.add_agent();
        assert_eq!(editor.agents.len(), initial_count + 1);
        assert_eq!(editor.selected_agent, initial_count);

        // Move up
        editor.move_agent_up();
        assert_eq!(editor.selected_agent, initial_count - 1);

        // Move down
        editor.move_agent_down();
        assert_eq!(editor.selected_agent, initial_count);

        // Remove agent
        editor.remove_agent();
        assert_eq!(editor.agents.len(), initial_count);

        // Cannot remove last agent if only 1 remains
        editor.agents.truncate(1);
        editor.selected_agent = 0;
        editor.remove_agent();
        assert_eq!(editor.agents.len(), 1, "cannot delete sole remaining agent");
    }

    #[test]
    fn test_pattern_editor_topic_add_and_remove() {
        let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
        let mut editor = PatternEditorState::from_pattern(&pat, PatternSource::Project);
        let init_topics = editor.topics.len();

        editor.add_topic();
        assert_eq!(editor.topics.len(), init_topics + 1);
        assert_eq!(editor.selected_topic, init_topics);

        editor.remove_topic();
        assert_eq!(editor.topics.len(), init_topics);

        editor.topics.truncate(1);
        editor.selected_topic = 0;
        editor.remove_topic();
        assert_eq!(editor.topics.len(), 1, "cannot delete sole remaining topic");
    }

    #[test]
    fn test_patterns_section_hit_testing_mouse_clicks() {
        let mut state = ShepherdPatternsSectionState::default();
        let palette = Palette::catppuccin();
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);

        let hits = render_patterns_section(&mut buffer, area, &state, &palette);

        // Click on second row
        if let Some(&(row_rect, _)) = hits.list_hits.rows.get(1) {
            let (handled, _) = state.handle_mouse(row_rect.x + 1, row_rect.y, &hits);
            assert!(handled);
            assert_eq!(state.selected, 1);
        }

        // Click on filter tab
        if let Some(&(tab_rect, tab_idx)) = hits.list_hits.filter_tabs.get(2) {
            let (handled, _) = state.handle_mouse(tab_rect.x + 1, tab_rect.y, &hits);
            assert!(handled);
            assert_eq!(state.filter_index, tab_idx);
        }

        // Start edit and test editor hits
        state.start_edit();
        let mut buffer2 = Buffer::empty(area);
        let edit_hits = render_patterns_section(&mut buffer2, area, &state, &palette);
        assert!(edit_hits.editor_hits.is_some());

        let eh = edit_hits.editor_hits.as_ref().unwrap();
        // Click on cancel button
        if let Some(cancel_rect) = eh.cancel_button {
            let (handled, _) = state.handle_mouse(cancel_rect.x + 1, cancel_rect.y, &edit_hits);
            assert!(handled);
            assert!(!state.has_sub_action(), "cancel button closes editor");
        }
    }

    #[test]
    fn test_pattern_operations_with_isolated_tempdirs() {
        use shepherd_core::patterns::{delete_pattern, load_pattern, save_pattern};

        let temp_base = std::env::temp_dir().join(format!(
            "shepherd-patterns-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let proj_root = temp_base.join("repo");
        let glob_dir = temp_base.join("config");
        std::fs::create_dir_all(&proj_root).unwrap();
        std::fs::create_dir_all(&glob_dir).unwrap();

        // 1. Create a custom pattern
        let mut pat = get_builtin_pattern("orchestrator-worker").unwrap();
        pat.name = "custom-flow".into();
        pat.description = "Custom orchestrator with modified worker vendor".into();
        if let Some(worker) = pat.agents.get_mut(1) {
            worker.vendor = Some("codex".into());
            worker.prompt = Some("Custom persona for worker".into());
        }

        // 2. Save pattern to project
        let path = save_pattern(&pat, false, &proj_root, Some(&glob_dir)).expect("save pattern succeeds");
        assert!(path.is_file());

        // 3. Load pattern and verify customized agent fields
        let (loaded, src, loaded_path) = load_pattern("custom-flow", &proj_root, Some(&glob_dir))
            .expect("load saved pattern succeeds");
        assert_eq!(src, PatternSource::Project);
        assert_eq!(loaded_path.unwrap(), path);
        assert_eq!(loaded.name, "custom-flow");
        assert_eq!(loaded.agents[1].vendor.as_deref(), Some("codex"));
        assert_eq!(loaded.agents[1].prompt.as_deref(), Some("Custom persona for worker"));

        // 4. Delete pattern from project
        let deleted_src = delete_pattern("custom-flow", &proj_root, Some(&glob_dir))
            .expect("delete pattern succeeds");
        assert_eq!(deleted_src, PatternSource::Project);
        assert!(!path.exists(), "pattern file must be deleted");

        // 5. Pattern no longer exists
        assert!(load_pattern("custom-flow", &proj_root, Some(&glob_dir)).is_err());

        let _ = std::fs::remove_dir_all(&temp_base);
    }

    #[test]
    fn test_pattern_run_execution_flow_via_key_and_mouse() {
        let mut state = ShepherdPatternsSectionState::default();
        let pat = get_builtin_pattern("orchestrator-worker").expect("template exists");
        state.update_detail(PatternDetail {
            pattern: pat,
            source: PatternSource::Builtin,
            path: None,
            problems: vec![],
        });
        state.patterns = vec![PatternSummary {
            name: "orchestrator-worker".into(),
            description: "OW".into(),
            kind: PatternKind::OrchestratorWorker,
            source: PatternSource::Builtin,
            agents_count: 4,
            topics_count: 2,
            shadowed: false,
            path: None,
        }];

        // Press 'r' to start execution form
        let (handled, act) = state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(handled);
        assert!(act.is_none());
        assert!(matches!(state.sub_action, Some(PatternSubAction::Run(_))));

        // Type task description
        for c in "Implement F8d".chars() {
            state.handle_key(&crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ));
        }

        // Tab to worktree field
        state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));

        // Type worktree path
        for c in "/tmp/wt-f8d".chars() {
            state.handle_key(&crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ));
        }

        // Render pure section and verify run_form_hits are populated
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();
        let hits = render_patterns_section(&mut buffer, area, &state, &palette);
        assert!(hits.run_form_hits.is_some());

        // Press Enter to submit execution
        let (handled_submit, act_submit) = state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(handled_submit);
        assert_eq!(
            act_submit,
            Some(ShepherdPatternsAction::RunPattern {
                pattern: "orchestrator-worker".into(),
                task: Some("Implement F8d".into()),
                worktree: Some("/tmp/wt-f8d".into()),
            })
        );
        assert!(state.sub_action.is_none());
    }
}
