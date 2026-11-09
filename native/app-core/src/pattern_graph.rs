//! A pattern as a graph: agents and topics as nodes, "publishes" and "consumes" as arrows, laid out in
//! columns left to right. No drawing here, only positions, so the layout is tested without a window.

use shepherd_core::config;
use shepherd_core::patterns::{self, Pattern, Problem, Severity};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

pub const NODE_W: f32 = 180.0;
pub const NODE_H: f32 = 58.0;
const GAP_X: f32 = 56.0;
const GAP_Y: f32 = 26.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Agent,
    Topic,
}

#[derive(Debug, Clone)]
pub struct Node {
    /// `agent:<id>` / `topic:<name>`
    pub key: String,
    pub kind: NodeKind,
    pub label: String,
    pub detail: String,
    pub start: bool,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub width: f32,
    pub height: f32,
}

/// A loaded pattern with what the editor shows next to it.
#[derive(Debug, Clone)]
pub struct PatternView {
    pub pattern: Pattern,
    pub source: patterns::PatternSource,
    pub path: Option<String>,
    /// `(error?, where, message)`
    pub problems: Vec<(bool, String, String)>,
    pub graph: Graph,
}

pub fn load(root: &Path, name: &str) -> Result<PatternView, String> {
    let (pattern, source, path) =
        patterns::load_pattern(name, root, None).map_err(|e| e.to_string())?;
    let cfg = config::load_config(root).map_err(|e| e.to_string())?;
    let problems = patterns::validate(&pattern, &cfg)
        .into_iter()
        .map(
            |Problem {
                 severity,
                 path,
                 message,
             }| (matches!(severity, Severity::Error), path, message),
        )
        .collect();
    let graph = layout(&pattern);
    Ok(PatternView {
        source,
        path: path.map(|p| p.display().to_string()),
        problems,
        graph,
        pattern,
    })
}

/// A fresh layout of `pattern` that keeps where the user already put the nodes that still exist.
pub fn relayout_keep(old: &Graph, pattern: &Pattern) -> Graph {
    let mut graph = layout(pattern);
    for node in &mut graph.nodes {
        if let Some(previous) = old.nodes.iter().find(|n| n.key == node.key) {
            (node.x, node.y) = (previous.x, previous.y);
        }
    }
    let (w, h) = graph.nodes.iter().fold((0.0f32, 0.0f32), |(w, h), n| {
        (w.max(n.x + NODE_W), h.max(n.y + NODE_H))
    });
    graph.width = graph.width.max(w + 20.0);
    graph.height = graph.height.max(h + 20.0);
    graph
}

/// Agents that consume nothing start the layering; a pattern whose agents all consume (a loop) starts at its first agent.
pub fn layout(pattern: &Pattern) -> Graph {
    let starters: HashSet<String> = pattern.starter_ids().into_iter().collect();
    let mut nodes: Vec<Node> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for agent in &pattern.agents {
        index.insert(format!("agent:{}", agent.id), nodes.len());
        let mut detail = agent.role.clone();
        if agent.count > 1 {
            detail.push_str(&format!(" ×{}", agent.count));
        }
        nodes.push(Node {
            key: format!("agent:{}", agent.id),
            kind: NodeKind::Agent,
            label: agent.id.clone(),
            detail,
            start: starters.contains(&agent.id),
            x: 0.0,
            y: 0.0,
        });
    }
    for topic in &pattern.topics {
        index.insert(format!("topic:{}", topic.name), nodes.len());
        nodes.push(Node {
            key: format!("topic:{}", topic.name),
            kind: NodeKind::Topic,
            label: format!("#{}", topic.name),
            detail: format!("{} partition(s)", topic.partitions),
            start: false,
            x: 0.0,
            y: 0.0,
        });
    }
    let mut edges = Vec::new();
    for agent in &pattern.agents {
        let from_agent = index[&format!("agent:{}", agent.id)];
        for topic in &agent.publishes {
            if let Some(&t) = index.get(&format!("topic:{topic}")) {
                edges.push(Edge {
                    from: from_agent,
                    to: t,
                });
            }
        }
        for topic in &agent.consumes {
            if let Some(&t) = index.get(&format!("topic:{topic}")) {
                edges.push(Edge {
                    from: t,
                    to: from_agent,
                });
            }
        }
    }

    // depth = distance from the roots along the arrows
    let consumes_nothing =
        |n: usize| !edges.iter().any(|e| e.to == n) && nodes[n].kind == NodeKind::Agent;
    let mut roots: Vec<usize> = (0..nodes.len()).filter(|&n| consumes_nothing(n)).collect();
    if roots.is_empty() {
        roots = (0..nodes.len()).take(1).collect();
    }
    let mut depth: HashMap<usize, usize> = roots.iter().map(|&r| (r, 0)).collect();
    let mut queue: VecDeque<usize> = roots.into_iter().collect();
    while let Some(n) = queue.pop_front() {
        for e in edges.iter().filter(|e| e.from == n) {
            if !depth.contains_key(&e.to) {
                depth.insert(e.to, depth[&n] + 1);
                queue.push_back(e.to);
            }
        }
    }
    let deepest = depth.values().copied().max().unwrap_or(0);
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); deepest + 2];
    for n in 0..nodes.len() {
        columns[depth.get(&n).copied().unwrap_or(deepest + 1)].push(n); // unreachable nodes go last
    }
    columns.retain(|c| !c.is_empty());
    let tallest = columns.iter().map(Vec::len).max().unwrap_or(1) as f32;
    let height = tallest * (NODE_H + GAP_Y) - GAP_Y + 20.0;
    for (col, members) in columns.iter().enumerate() {
        let used = members.len() as f32 * (NODE_H + GAP_Y) - GAP_Y;
        for (row, &n) in members.iter().enumerate() {
            nodes[n].x = 10.0 + col as f32 * (NODE_W + GAP_X);
            nodes[n].y = 10.0 + (height - 20.0 - used) / 2.0 + row as f32 * (NODE_H + GAP_Y);
        }
    }
    Graph {
        width: columns.len() as f32 * (NODE_W + GAP_X) - GAP_X + 20.0,
        height,
        nodes,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_core::patterns::get_builtin_pattern;

    #[test]
    fn a_pipeline_lays_out_left_to_right_and_a_loop_still_terminates() {
        let pattern = get_builtin_pattern("orchestrator-worker").expect("builtin pattern");
        let graph = layout(&pattern);
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Agent)
                .count(),
            pattern.agents.len()
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Topic)
                .count(),
            pattern.topics.len()
        );
        assert!(!graph.edges.is_empty());
        for n in &graph.nodes {
            assert!(
                n.x >= 0.0
                    && n.y >= 0.0
                    && n.x + NODE_W <= graph.width + 1.0
                    && n.y + NODE_H <= graph.height + 1.0,
                "{} is outside {}x{}",
                n.label,
                graph.width,
                graph.height
            );
        }
        // every edge goes to a different place than it starts
        assert!(graph.edges.iter().all(|e| e.from != e.to));
    }

    #[test]
    fn moved_nodes_stay_where_they_were_after_the_pattern_changes() {
        let mut pattern = get_builtin_pattern("blackboard").unwrap();
        let mut graph = layout(&pattern);
        graph.nodes[0].x = 500.0;
        graph.nodes[0].y = 300.0;
        let moved = graph.nodes[0].key.clone();
        pattern.topics.push(shepherd_core::patterns::Topic {
            name: "extra".into(),
            partitions: 1,
            key: None,
            filter: None,
        });
        let after = relayout_keep(&graph, &pattern);
        let node = after.nodes.iter().find(|n| n.key == moved).unwrap();
        assert_eq!((node.x, node.y), (500.0, 300.0));
        assert!(after.width >= 500.0 + NODE_W, "the canvas grows to hold it");
        assert!(after.nodes.iter().any(|n| n.key == "topic:extra"));
    }

    #[test]
    fn the_four_builtin_topologies_all_lay_out() {
        for name in [
            "orchestrator-worker",
            "hierarchical",
            "blackboard",
            "market",
        ] {
            let pattern = get_builtin_pattern(name).unwrap_or_else(|| panic!("{name} missing"));
            let graph = layout(&pattern);
            assert!(graph.width > 0.0 && graph.height > 0.0, "{name}");
            // no two nodes on the same spot
            let mut spots: Vec<(i32, i32)> = graph
                .nodes
                .iter()
                .map(|n| (n.x as i32, n.y as i32))
                .collect();
            spots.sort();
            spots.dedup();
            assert_eq!(spots.len(), graph.nodes.len(), "{name} overlaps");
        }
    }
}
