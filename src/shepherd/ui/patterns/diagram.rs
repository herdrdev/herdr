//! ASCII topology diagram generation for multi-agent patterns.

use shepherd_core::patterns::{Pattern, PatternAgent, PatternKind};

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

pub(crate) fn truncate_line(s: &str, max_w: usize) -> String {
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
    let mid = format!(
        "│{}{}{}│",
        " ".repeat(left_pad),
        truncated_text,
        " ".repeat(right_pad)
    );
    (top, mid, bot)
}

fn render_orchestrator_worker_diagram(
    pattern: &Pattern,
    narrow: bool,
    max_w: usize,
) -> Vec<String> {
    let mut lines = Vec::new();

    // Identify orchestrator and worker
    let orchestrator = pattern
        .agents
        .iter()
        .find(|a| {
            a.role.to_lowercase() == "brain"
                || a.role.to_lowercase() == "planner"
                || a.group.is_none()
        })
        .or_else(|| pattern.agents.first());

    let worker = pattern
        .agents
        .iter()
        .find(|a| a.role.to_lowercase() == "coder" || a.group.is_some() || a.count > 1)
        .or_else(|| pattern.agents.get(1));

    let orch_role = orchestrator.map(|a| a.role.as_str()).unwrap_or("brain");
    let orch_count = orchestrator.map(|a| a.count).unwrap_or(1);
    let orch_pub = orchestrator
        .and_then(|a| a.publishes.first().cloned())
        .unwrap_or_else(|| "orders".into());
    let orch_sub = orchestrator
        .and_then(|a| a.consumes.first().cloned())
        .unwrap_or_else(|| "results".into());

    let worker_role = worker.map(|a| a.role.as_str()).unwrap_or("coder");
    let worker_count = worker.map(|a| a.count).unwrap_or(3);
    let worker_group = worker
        .and_then(|a| a.group.as_deref())
        .unwrap_or("worker-pool");

    let orders_topic = pattern.topics.iter().find(|t| t.name == orch_pub);
    let orders_p = orders_topic.map(|t| t.partitions).unwrap_or(4);

    let results_topic = pattern.topics.iter().find(|t| t.name == orch_sub);
    let results_p = results_topic.map(|t| t.partitions).unwrap_or(1);

    if narrow {
        lines.push(format!("[Orquestador] {orch_role} (x{orch_count})"));
        lines.push(format!("  │ publica: {orch_pub}"));
        lines.push("  ▼".into());
        lines.push(format!("[Topic: {orch_pub} (p:{orders_p})]"));
        lines.push(format!("  │ consume: {worker_group}"));
        lines.push("  ▼".into());
        lines.push(format!("[Workers] {worker_role} (x{worker_count})"));
        lines.push(format!("  │ publica: {orch_sub}"));
        lines.push("  ▼".into());
        lines.push(format!("[Topic: {orch_sub} (p:{results_p})]"));
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

        let top_txt = format!("[Topic: {} (p:{})]", orch_pub, orders_p);
        let top_pad = " ".repeat(center.saturating_sub(top_txt.chars().count() / 2));
        lines.push(format!("{}{}", top_pad, top_txt));
        lines.push(format!("{}│ consume: {}", indent, worker_group));
        lines.push(format!("{}▼", indent));

        let worker_txt = format!(
            "[Worker Pool] {} (x{}) [group: {}]",
            worker_role, worker_count, worker_group
        );
        let (t2, m2, b2) = box_line(&worker_txt, box_w);
        lines.push(t2);
        lines.push(m2);
        lines.push(b2);
        lines.push(format!("{}│ publica: {}", indent, orch_sub));
        lines.push(format!("{}▼", indent));

        let res_txt = format!("[Topic: {} (p:{})]", orch_sub, results_p);
        let res_pad = " ".repeat(center.saturating_sub(res_txt.chars().count() / 2));
        lines.push(format!("{}{}", res_pad, res_txt));
        lines.push(format!("{}│ consume", indent));
        lines.push(format!("{}▼", indent));

        let return_txt = format!("[Orquestador: {}]", orch_role);
        let ret_pad = " ".repeat(center.saturating_sub(return_txt.chars().count() / 2));
        lines.push(format!("{}{}", ret_pad, return_txt));
    }

    lines
        .into_iter()
        .map(|l| truncate_line(&l, max_w))
        .collect()
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
        lines.push("[Hierarchical with no agents]".into());
        return lines;
    }

    if narrow {
        for (idx, (lvl, ags)) in levels.iter().enumerate() {
            let role_desc = ags
                .iter()
                .map(|a| format!("{} (x{})", a.role, a.count))
                .collect::<Vec<_>>()
                .join(", ");
            let label = match lvl {
                0 => "Root / Director",
                1 => "Managers",
                _ => "Especialistas",
            };
            lines.push(format!("[N{}: {}] {}", lvl, label, role_desc));
            if idx + 1 < levels.len() {
                let pub_topic = ags
                    .first()
                    .and_then(|a| a.publishes.first())
                    .cloned()
                    .unwrap_or_else(|| "orders".into());
                lines.push(format!("  │ publica: {pub_topic}"));
                lines.push("  ▼".into());
                lines.push(format!("[Topic: {pub_topic}]"));
                lines.push("  │ consume".into());
                lines.push("  ▼".into());
            }
        }
        lines.push("  ◄── returns via result topics".into());
    } else {
        let box_w = max_w.min(52).max(36);
        for (idx, (lvl, ags)) in levels.iter().enumerate() {
            let role_desc = ags
                .iter()
                .map(|a| format!("{} (x{})", a.role, a.count))
                .collect::<Vec<_>>()
                .join(", ");
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
                let pub_topic = ags
                    .first()
                    .and_then(|a| a.publishes.first())
                    .cloned()
                    .unwrap_or_else(|| "cascade-orders".into());
                lines.push(format!("{}│ publica: {}", indent, pub_topic));
                lines.push(format!("{}▼", indent));

                let top_txt = format!("[Topic: {}]", pub_topic);
                let top_pad = " ".repeat(center.saturating_sub(top_txt.chars().count() / 2));
                lines.push(format!("{}{}", top_pad, top_txt));
                lines.push(format!("{}│ consume", indent));
                lines.push(format!("{}▼", indent));
            }
        }
        lines.push(
            "  ▲── Results cascading back up (team-results ──► root-results) ──► Director".into(),
        );
    }

    lines
        .into_iter()
        .map(|l| truncate_line(&l, max_w))
        .collect()
}

fn render_blackboard_diagram(pattern: &Pattern, narrow: bool, max_w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let topic_name = pattern
        .topics
        .first()
        .map(|t| t.name.as_str())
        .unwrap_or("blackboard");
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

    lines
        .into_iter()
        .map(|l| truncate_line(&l, max_w))
        .collect()
}

fn render_market_diagram(pattern: &Pattern, narrow: bool, max_w: usize) -> Vec<String> {
    let mut lines = Vec::new();

    let mm = pattern
        .agents
        .iter()
        .find(|a| a.role.to_lowercase() == "brain" || a.id.contains("market"))
        .map(|a| a.role.as_str())
        .unwrap_or("brain");
    let req = pattern
        .agents
        .iter()
        .find(|a| a.publishes.iter().any(|t| t == "ask"))
        .map(|a| a.role.as_str())
        .unwrap_or("docs");
    let sol = pattern
        .agents
        .iter()
        .find(|a| a.publishes.iter().any(|t| t == "bid"))
        .map(|a| a.role.as_str())
        .unwrap_or("coder");

    if narrow {
        lines.push(format!("[Requesters: {req}]"));
        lines.push("  │ publica demanda".into());
        lines.push("  ▼".into());
        lines.push("[Topic: ask (p:1)]".into());
        lines.push("  │ consume".into());
        lines.push("  ▼".into());
        lines.push(format!("[Solvers: {sol}]"));
        lines.push("  │ oferta competitiva".into());
        lines.push("  ▼".into());
        lines.push("[Topic: bid (p:1)]".into());
        lines.push("  │ consume".into());
        lines.push("  ▼".into());
        lines.push(format!("[Market-Maker: {mm}]"));
        lines.push("  │ adjudica (transaction)".into());
        lines.push("  ▼".into());
        lines.push("[Topic: transaction] ──► [Solvers / Requesters]".into());
    } else {
        lines.push(format!(
            "[Requesters: {req}] ──────► [Topic: ask] ──────► [Solvers: {sol}]"
        ));
        lines.push(
            "                                                          │ ofertas competitivas"
                .into(),
        );
        lines.push("                                                          ▼".into());
        lines.push(format!(
            "[Market-Maker: {mm}] ◄────────── [Topic: bid] ───────────┘"
        ));
        lines.push("         │ matches and awards transactions".into());
        lines.push("         ▼".into());
        lines.push("[Topic: transaction] ──────► [Execution on Solvers and Requesters]".into());
    }

    lines
        .into_iter()
        .map(|l| truncate_line(&l, max_w))
        .collect()
}
