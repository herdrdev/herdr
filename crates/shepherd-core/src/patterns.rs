//! Multi-agent event-driven patterns: model, storage, validation, templates, and helpers.
//!
//! Based on Confluent's "A Guide to Event-Driven Design for Agents and Multi-Agent Systems" (Chapter III).

use crate::config::{available_vendors, Config};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

// --- Embedded Templates ---

const TEMPLATE_ORCHESTRATOR_WORKER: &str =
    include_str!("../assets/patterns/orchestrator-worker.yaml");
const TEMPLATE_HIERARCHICAL: &str = include_str!("../assets/patterns/hierarchical.yaml");
const TEMPLATE_BLACKBOARD: &str = include_str!("../assets/patterns/blackboard.yaml");
const TEMPLATE_MARKET: &str = include_str!("../assets/patterns/market.yaml");

pub struct EmbeddedTemplate {
    pub name: &'static str,
    pub yaml: &'static str,
}

pub const EMBEDDED_TEMPLATES: &[EmbeddedTemplate] = &[
    EmbeddedTemplate {
        name: "orchestrator-worker",
        yaml: TEMPLATE_ORCHESTRATOR_WORKER,
    },
    EmbeddedTemplate {
        name: "hierarchical",
        yaml: TEMPLATE_HIERARCHICAL,
    },
    EmbeddedTemplate {
        name: "blackboard",
        yaml: TEMPLATE_BLACKBOARD,
    },
    EmbeddedTemplate {
        name: "market",
        yaml: TEMPLATE_MARKET,
    },
];

pub fn get_builtin_template(name: &str) -> Option<&'static str> {
    EMBEDDED_TEMPLATES
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.yaml)
}

pub fn is_builtin_template(name: &str) -> bool {
    get_builtin_template(name).is_some()
}

// --- Data Models ---

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatternKind {
    OrchestratorWorker,
    Hierarchical,
    Blackboard,
    Market,
}

impl PatternKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PatternKind::OrchestratorWorker => "orchestrator-worker",
            PatternKind::Hierarchical => "hierarchical",
            PatternKind::Blackboard => "blackboard",
            PatternKind::Market => "market",
        }
    }
}

impl fmt::Display for PatternKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for PatternKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "orchestrator-worker" => Ok(PatternKind::OrchestratorWorker),
            "hierarchical" => Ok(PatternKind::Hierarchical),
            "blackboard" => Ok(PatternKind::Blackboard),
            "market" => Ok(PatternKind::Market),
            other => Err(format!("unknown pattern kind '{other}'")),
        }
    }
}

fn default_partitions() -> u32 {
    1
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    #[serde(default = "default_partitions")]
    pub partitions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

fn default_count() -> u32 {
    1
}

fn is_default_count(c: &u32) -> bool {
    *c == 1
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PatternAgent {
    pub id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default = "default_count", skip_serializing_if = "is_default_count")]
    pub count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publishes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub name: String,
    pub kind: PatternKind,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<Topic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<PatternAgent>,
}

// --- Storage Metadata & Enums ---

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatternSource {
    Project,
    Global,
    Builtin,
}

impl PatternSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PatternSource::Project => "project",
            PatternSource::Global => "global",
            PatternSource::Builtin => "builtin",
        }
    }
}

impl fmt::Display for PatternSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PatternSummary {
    pub name: String,
    pub kind: PatternKind,
    pub description: String,
    pub source: PatternSource,
    pub shadowed: bool,
    pub agents_count: usize,
    pub topics_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatternDetail {
    #[serde(flatten)]
    pub pattern: Pattern,
    pub source: PatternSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub problems: Vec<Problem>,
}

// --- Validation Models ---

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

impl Problem {
    pub fn error(path: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: msg.into(),
        }
    }

    pub fn warning(path: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: msg.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

// --- Error Type ---

#[derive(Debug)]
pub enum PatternError {
    InvalidName(String),
    NotFound(String),
    CannotDeleteBuiltin(String),
    AlreadyExists(String),
    Io(std::io::Error),
    Serialization(String),
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::InvalidName(msg) => write!(f, "invalid pattern name: {msg}"),
            PatternError::NotFound(name) => write!(f, "pattern '{name}' not found"),
            PatternError::CannotDeleteBuiltin(name) => {
                write!(f, "cannot delete built-in pattern template '{name}'")
            }
            PatternError::AlreadyExists(name) => write!(f, "pattern '{name}' already exists"),
            PatternError::Io(err) => write!(f, "I/O error: {err}"),
            PatternError::Serialization(msg) => write!(f, "serialization error: {msg}"),
        }
    }
}

impl std::error::Error for PatternError {}

impl From<std::io::Error> for PatternError {
    fn from(err: std::io::Error) -> Self {
        PatternError::Io(err)
    }
}

// --- Validation Logic ---

const MAX_PROMPT_LEN: usize = 16384;
const MAX_AGENTS: usize = 64;
const MAX_AGENT_COUNT: u32 = 32;
const MAX_PARTITIONS: u32 = 256;

pub fn is_valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn validate_pattern_name(name: &str) -> Result<(), PatternError> {
    if name.is_empty() {
        return Err(PatternError::InvalidName("name cannot be empty".into()));
    }
    if !is_valid_ident(name) {
        return Err(PatternError::InvalidName(format!(
            "name '{name}' contains invalid characters (use alphanumeric, dash or underscore)"
        )));
    }
    Ok(())
}

/// Validate a Pattern against system limits, semantic integrity, and active Config.
pub fn validate(pattern: &Pattern, config: &Config) -> Vec<Problem> {
    let mut problems = Vec::new();

    // 1. Pattern Name & Description
    if pattern.name.trim().is_empty() {
        problems.push(Problem::error("name", "pattern name cannot be empty"));
    } else if !is_valid_ident(&pattern.name) {
        problems.push(Problem::error(
            "name",
            format!(
                "pattern name '{}' contains invalid characters (alphanumeric, dash or underscore allowed)",
                pattern.name
            ),
        ));
    }
    if pattern.description.trim().is_empty() {
        problems.push(Problem::warning(
            "description",
            "pattern description is empty",
        ));
    }

    // 2. Topics definition & limits
    let mut topic_names = HashSet::new();
    for (i, topic) in pattern.topics.iter().enumerate() {
        let path = format!("topics[{i}]");
        if topic.name.trim().is_empty() {
            problems.push(Problem::error(
                format!("{path}.name"),
                "topic name cannot be empty",
            ));
        } else if !is_valid_ident(&topic.name) {
            problems.push(Problem::error(
                format!("{path}.name"),
                format!("topic name '{}' contains invalid characters", topic.name),
            ));
        } else if !topic_names.insert(topic.name.clone()) {
            problems.push(Problem::error(
                format!("{path}.name"),
                format!("duplicate topic name '{}'", topic.name),
            ));
        }

        if topic.partitions == 0 {
            problems.push(Problem::error(
                format!("{path}.partitions"),
                "topic partitions must be at least 1",
            ));
        } else if topic.partitions > MAX_PARTITIONS {
            problems.push(Problem::error(
                format!("{path}.partitions"),
                format!("topic partitions exceeds maximum ({MAX_PARTITIONS})"),
            ));
        }
    }

    // 3. Agent definitions & limits
    if pattern.agents.is_empty() {
        problems.push(Problem::error(
            "agents",
            "pattern must declare at least one agent",
        ));
    } else if pattern.agents.len() > MAX_AGENTS {
        problems.push(Problem::error(
            "agents",
            format!("pattern exceeds maximum agent definitions ({MAX_AGENTS})"),
        ));
    }

    let mut agent_ids = HashSet::new();
    let mut topics_published = HashSet::new();
    let mut topics_consumed = HashSet::new();
    let available_vendors_list = available_vendors(config);

    for (i, agent) in pattern.agents.iter().enumerate() {
        let path = format!("agents[{i}]");

        // Agent ID
        if agent.id.trim().is_empty() {
            problems.push(Problem::error(
                format!("{path}.id"),
                "agent id cannot be empty",
            ));
        } else if !is_valid_ident(&agent.id) {
            problems.push(Problem::error(
                format!("{path}.id"),
                format!("agent id '{}' contains invalid characters", agent.id),
            ));
        } else if !agent_ids.insert(agent.id.clone()) {
            problems.push(Problem::error(
                format!("{path}.id"),
                format!("duplicate agent id '{}'", agent.id),
            ));
        }

        // Count limit
        if agent.count == 0 {
            problems.push(Problem::error(
                format!("{path}.count"),
                "agent count must be at least 1",
            ));
        } else if agent.count > MAX_AGENT_COUNT {
            problems.push(Problem::error(
                format!("{path}.count"),
                format!("agent count exceeds maximum ({MAX_AGENT_COUNT})"),
            ));
        }

        // Persona prompt limit
        if let Some(ref prompt) = agent.prompt {
            if prompt.len() > MAX_PROMPT_LEN {
                problems.push(Problem::error(
                    format!("{path}.prompt"),
                    format!(
                        "agent persona prompt length ({} chars) exceeds maximum ({MAX_PROMPT_LEN} chars)",
                        prompt.len()
                    ),
                ));
            }
        }

        // Role verification
        let role_def = config.roles.get(&agent.role);
        if role_def.is_none() {
            problems.push(Problem::error(
                format!("{path}.role"),
                format!(
                    "unknown role '{}' (not defined in config roles)",
                    agent.role
                ),
            ));
        }

        // Vendor verification
        let effective_vendor = agent
            .vendor
            .as_deref()
            .or_else(|| role_def.map(|r| r.vendor.as_str()));

        if let Some(vendor) = effective_vendor {
            if !vendor.is_empty() {
                if let Some(vendor_cfg) = config.vendors.get(vendor) {
                    if !available_vendors_list.iter().any(|v| v == vendor) {
                        problems.push(Problem::warning(
                            format!("{path}.vendor"),
                            format!(
                                "vendor '{vendor}' is configured but its command '{}' is not installed or not in PATH",
                                vendor_cfg.command
                            ),
                        ));
                    }
                    // Model verification
                    let effective_model = agent
                        .model
                        .as_deref()
                        .or_else(|| role_def.map(|r| r.model.as_str()))
                        .unwrap_or("");
                    if !effective_model.is_empty()
                        && !vendor_cfg.models.is_empty()
                        && !vendor_cfg.models.iter().any(|m| m == effective_model)
                    {
                        problems.push(Problem::warning(
                            format!("{path}.model"),
                            format!(
                                "model '{effective_model}' is not in known models list for vendor '{vendor}'"
                            ),
                        ));
                    }
                } else {
                    problems.push(Problem::warning(
                        format!("{path}.vendor"),
                        format!("vendor '{vendor}' is not defined in config vendors"),
                    ));
                }
            }
        }

        // Topics published
        for (j, pub_topic) in agent.publishes.iter().enumerate() {
            if !topic_names.contains(pub_topic) {
                problems.push(Problem::error(
                    format!("{path}.publishes[{j}]"),
                    format!(
                        "agent '{}' publishes to unknown topic '{pub_topic}'",
                        agent.id
                    ),
                ));
            } else {
                topics_published.insert(pub_topic.clone());
            }
        }

        // Topics consumed
        for (j, con_topic) in agent.consumes.iter().enumerate() {
            if !topic_names.contains(con_topic) {
                problems.push(Problem::error(
                    format!("{path}.consumes[{j}]"),
                    format!("agent '{}' consumes unknown topic '{con_topic}'", agent.id),
                ));
            } else {
                topics_consumed.insert(con_topic.clone());
            }
        }
    }

    // 4. Orphan topics check (Warning)
    for (i, topic) in pattern.topics.iter().enumerate() {
        if !topics_published.contains(&topic.name) && !topics_consumed.contains(&topic.name) {
            problems.push(Problem::warning(
                format!("topics[{i}].name"),
                format!(
                    "topic '{}' is declared but neither published nor consumed by any agent",
                    topic.name
                ),
            ));
        }
    }

    // 5. Pattern-Specific Topologies
    match pattern.kind {
        PatternKind::OrchestratorWorker => {
            validate_orchestrator_worker(pattern, &mut problems);
        }
        PatternKind::Hierarchical => {
            validate_hierarchical(pattern, &mut problems);
        }
        PatternKind::Blackboard => {
            validate_blackboard(pattern, &mut problems);
        }
        PatternKind::Market => {
            validate_market(pattern, &mut problems);
        }
    }

    problems
}

fn validate_orchestrator_worker(pattern: &Pattern, problems: &mut Vec<Problem>) {
    // 1 orchestrator (publishes orders, consumes responses)
    // >= 1 worker (consumes orders, publishes responses, has group)
    let mut orchestrators = Vec::new();
    let mut workers = Vec::new();

    for (i, agent) in pattern.agents.iter().enumerate() {
        let publishes_set: HashSet<&str> = agent.publishes.iter().map(String::as_str).collect();

        // Check if agent could be worker: consumes something and publishes something back, and has group
        if agent.group.is_some() {
            workers.push(i);
        } else if !publishes_set.is_empty() {
            orchestrators.push(i);
        }
    }

    if orchestrators.is_empty() {
        problems.push(Problem::error(
            "agents",
            "orchestrator-worker pattern requires exactly 1 orchestrator agent (an agent publishing orders)",
        ));
    } else if orchestrators.len() > 1 {
        problems.push(Problem::error(
            "agents",
            format!(
                "orchestrator-worker pattern requires exactly 1 orchestrator agent, found {}",
                orchestrators.len()
            ),
        ));
    } else {
        let orch_idx = orchestrators[0];
        let orch = &pattern.agents[orch_idx];
        if orch.count != 1 {
            problems.push(Problem::error(
                format!("agents[{orch_idx}].count"),
                "orchestrator agent count must be 1",
            ));
        }
    }

    if workers.is_empty() {
        problems.push(Problem::error(
            "agents",
            "orchestrator-worker pattern requires at least 1 worker agent with a consumer group",
        ));
    }

    // Check that each worker has a non-empty consumer group and response topic
    if let Some(&orch_idx) = orchestrators.first() {
        let orch = &pattern.agents[orch_idx];
        let orch_publishes: HashSet<&str> = orch.publishes.iter().map(String::as_str).collect();
        let orch_consumes: HashSet<&str> = orch.consumes.iter().map(String::as_str).collect();

        let mut found_response_topic = false;
        for &w_idx in &workers {
            let worker = &pattern.agents[w_idx];
            let worker_consumes: HashSet<&str> =
                worker.consumes.iter().map(String::as_str).collect();
            let worker_publishes: HashSet<&str> =
                worker.publishes.iter().map(String::as_str).collect();

            // Worker consumes orders from orchestrator
            let consumes_orch_orders = worker_consumes
                .intersection(&orch_publishes)
                .next()
                .is_some();
            if !consumes_orch_orders {
                problems.push(Problem::warning(
                    format!("agents[{w_idx}].consumes"),
                    format!(
                        "worker '{}' does not consume any topic published by orchestrator '{}'",
                        worker.id, orch.id
                    ),
                ));
            }

            // Worker publishes responses back to orchestrator
            let publishes_to_orch = worker_publishes
                .intersection(&orch_consumes)
                .next()
                .is_some();
            if publishes_to_orch {
                found_response_topic = true;
            } else {
                problems.push(Problem::error(
                    format!("agents[{w_idx}].publishes"),
                    format!(
                        "worker '{}' does not publish to any response topic consumed by orchestrator '{}'",
                        worker.id, orch.id
                    ),
                ));
            }
        }

        if !found_response_topic {
            problems.push(Problem::error(
                "topics",
                "orchestrator-worker pattern requires a response topic consumed by orchestrator and published by workers",
            ));
        }
    }
}

fn validate_hierarchical(pattern: &Pattern, problems: &mut Vec<Problem>) {
    // Tree without cycles, single top (level 0), contiguous levels, each non-leaf with subtree
    let mut levels_map: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut max_level = 0;

    for (i, agent) in pattern.agents.iter().enumerate() {
        match agent.level {
            None => {
                problems.push(Problem::error(
                    format!("agents[{i}].level"),
                    format!(
                        "hierarchical pattern requires a level for agent '{}'",
                        agent.id
                    ),
                ));
            }
            Some(lvl) => {
                levels_map.entry(lvl).or_default().push(i);
                if lvl > max_level {
                    max_level = lvl;
                }
            }
        }
    }

    // Single top level 0
    match levels_map.get(&0) {
        None => {
            problems.push(Problem::error(
                "agents",
                "hierarchical pattern requires a root director agent at level 0",
            ));
        }
        Some(roots) => {
            if roots.len() > 1 {
                problems.push(Problem::error(
                    "agents",
                    format!(
                        "hierarchical pattern requires exactly 1 root agent at level 0, found {}",
                        roots.len()
                    ),
                ));
            } else if pattern.agents[roots[0]].count != 1 {
                problems.push(Problem::error(
                    format!("agents[{}].count", roots[0]),
                    "root agent at level 0 must have count 1",
                ));
            }
        }
    }

    // Contiguous levels check
    if max_level > 0 {
        for lvl in 1..=max_level {
            if !levels_map.contains_key(&lvl) {
                problems.push(Problem::error(
                    "agents",
                    format!(
                        "hierarchical pattern has a gap at level {lvl}; levels must be contiguous"
                    ),
                ));
            }
        }
    }

    // Each non-leaf node orchestrates down to level+1
    for lvl in 0..max_level {
        let agents_at_lvl = match levels_map.get(&lvl) {
            Some(list) => list,
            None => continue,
        };
        let agents_at_sub_lvl = match levels_map.get(&(lvl + 1)) {
            Some(list) => list,
            None => continue,
        };

        let sub_consumes: HashSet<&str> = agents_at_sub_lvl
            .iter()
            .flat_map(|&idx| pattern.agents[idx].consumes.iter().map(String::as_str))
            .collect();
        let sub_publishes: HashSet<&str> = agents_at_sub_lvl
            .iter()
            .flat_map(|&idx| pattern.agents[idx].publishes.iter().map(String::as_str))
            .collect();

        for &idx in agents_at_lvl {
            let agent = &pattern.agents[idx];
            let pub_set: HashSet<&str> = agent.publishes.iter().map(String::as_str).collect();
            let con_set: HashSet<&str> = agent.consumes.iter().map(String::as_str).collect();

            let orders_down = pub_set.intersection(&sub_consumes).next().is_some();
            let results_up = con_set.intersection(&sub_publishes).next().is_some();

            if !orders_down {
                problems.push(Problem::error(
                    format!("agents[{idx}].publishes"),
                    format!(
                        "non-leaf agent '{}' (level {lvl}) must publish orders consumed by agents at level {}",
                        agent.id,
                        lvl + 1
                    ),
                ));
            }
            if !results_up {
                problems.push(Problem::error(
                    format!("agents[{idx}].consumes"),
                    format!(
                        "non-leaf agent '{}' (level {lvl}) must consume responses published by agents at level {}",
                        agent.id,
                        lvl + 1
                    ),
                ));
            }
        }
    }
}

fn validate_blackboard(pattern: &Pattern, problems: &mut Vec<Problem>) {
    // >= 2 agents, >= 1 shared topic used by all agents
    let total_agents: u32 = pattern.agents.iter().map(|a| a.count).sum();
    if total_agents < 2 {
        problems.push(Problem::error(
            "agents",
            "blackboard pattern requires at least 2 agent instances",
        ));
    }

    if pattern.topics.is_empty() {
        problems.push(Problem::error(
            "topics",
            "blackboard pattern requires at least 1 blackboard topic",
        ));
        return;
    }

    // Check if there is at least one topic used by all agents
    let mut shared_found = false;
    for topic in &pattern.topics {
        let used_by_all = pattern.agents.iter().all(|agent| {
            agent.publishes.iter().any(|t| t == &topic.name)
                || agent.consumes.iter().any(|t| t == &topic.name)
        });
        if used_by_all {
            shared_found = true;
            break;
        }
    }

    if !shared_found {
        problems.push(Problem::error(
            "topics",
            "blackboard pattern requires at least one shared topic used (published or consumed) by all agents",
        ));
    }
}

fn validate_market(pattern: &Pattern, problems: &mut Vec<Problem>) {
    // 1 market-maker + >= 2 solvers + topics bid, ask, transaction
    let topic_names: HashSet<&str> = pattern.topics.iter().map(|t| t.name.as_str()).collect();
    for required in &["bid", "ask", "transaction"] {
        if !topic_names.contains(required) {
            problems.push(Problem::error(
                "topics",
                format!("market pattern requires '{required}' topic"),
            ));
        }
    }

    let mut market_makers = Vec::new();
    let mut solver_count: u32 = 0;

    for (i, agent) in pattern.agents.iter().enumerate() {
        let pub_set: HashSet<&str> = agent.publishes.iter().map(String::as_str).collect();
        let con_set: HashSet<&str> = agent.consumes.iter().map(String::as_str).collect();

        // Market-maker consumes bid and ask, publishes transaction
        if con_set.contains("bid") && con_set.contains("ask") && pub_set.contains("transaction") {
            market_makers.push(i);
        }

        // Solvers publish bid
        if pub_set.contains("bid") {
            solver_count += agent.count;
        }
    }

    if market_makers.is_empty() {
        problems.push(Problem::error(
            "agents",
            "market pattern requires exactly 1 market-maker agent consuming 'bid' and 'ask' and publishing 'transaction'",
        ));
    } else if market_makers.len() > 1 {
        problems.push(Problem::error(
            "agents",
            format!(
                "market pattern requires exactly 1 market-maker agent, found {}",
                market_makers.len()
            ),
        ));
    } else {
        let mm_idx = market_makers[0];
        if pattern.agents[mm_idx].count != 1 {
            problems.push(Problem::error(
                format!("agents[{mm_idx}].count"),
                "market-maker agent count must be 1",
            ));
        }
    }

    if solver_count < 2 {
        problems.push(Problem::error(
            "agents",
            format!(
                "market pattern requires at least 2 solver agent instances publishing to 'bid', found {solver_count}"
            ),
        ));
    }
}

// --- Storage Management ---

pub fn project_patterns_dir(project_root: &Path) -> PathBuf {
    project_root.join(".shepherd/patterns")
}

pub fn global_patterns_dir_with(global_config: Option<&Path>) -> PathBuf {
    if let Some(cfg) = global_config {
        return cfg.to_path_buf();
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(path).join("shepherd/patterns");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/shepherd/patterns")
}

pub fn list_patterns(
    project_root: &Path,
    global_dir_override: Option<&Path>,
) -> Result<Vec<PatternSummary>, PatternError> {
    let mut summaries = Vec::new();
    let mut project_names = HashSet::new();
    let mut global_names = HashSet::new();

    // 1. Project Patterns
    let proj_dir = project_patterns_dir(project_root);
    if proj_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&proj_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(pattern) = serde_yaml_ng::from_str::<Pattern>(&content) {
                            project_names.insert(pattern.name.clone());
                            summaries.push(PatternSummary {
                                name: pattern.name,
                                kind: pattern.kind,
                                description: pattern.description,
                                source: PatternSource::Project,
                                shadowed: false,
                                agents_count: pattern.agents.iter().map(|a| a.count as usize).sum(),
                                topics_count: pattern.topics.len(),
                                path: Some(path.display().to_string()),
                            });
                        }
                    }
                }
            }
        }
    }

    // 2. Global Patterns
    let glob_dir = global_patterns_dir_with(global_dir_override);
    if glob_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&glob_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(pattern) = serde_yaml_ng::from_str::<Pattern>(&content) {
                            let shadowed = project_names.contains(&pattern.name);
                            global_names.insert(pattern.name.clone());
                            summaries.push(PatternSummary {
                                name: pattern.name,
                                kind: pattern.kind,
                                description: pattern.description,
                                source: PatternSource::Global,
                                shadowed,
                                agents_count: pattern.agents.iter().map(|a| a.count as usize).sum(),
                                topics_count: pattern.topics.len(),
                                path: Some(path.display().to_string()),
                            });
                        }
                    }
                }
            }
        }
    }

    // 3. Builtin Patterns
    for tmpl in EMBEDDED_TEMPLATES {
        if let Ok(pattern) = serde_yaml_ng::from_str::<Pattern>(tmpl.yaml) {
            let shadowed = project_names.contains(tmpl.name) || global_names.contains(tmpl.name);
            summaries.push(PatternSummary {
                name: pattern.name,
                kind: pattern.kind,
                description: pattern.description,
                source: PatternSource::Builtin,
                shadowed,
                agents_count: pattern.agents.iter().map(|a| a.count as usize).sum(),
                topics_count: pattern.topics.len(),
                path: None,
            });
        }
    }

    summaries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.source.cmp(&b.source)));
    Ok(summaries)
}

pub fn load_pattern_from_file(path: &Path) -> Result<Pattern, PatternError> {
    let content = std::fs::read_to_string(path)?;
    let pattern: Pattern = serde_yaml_ng::from_str(&content)
        .map_err(|e| PatternError::Serialization(e.to_string()))?;
    Ok(pattern)
}

pub fn load_pattern(
    name: &str,
    project_root: &Path,
    global_dir_override: Option<&Path>,
) -> Result<(Pattern, PatternSource, Option<PathBuf>), PatternError> {
    validate_pattern_name(name)?;

    // 1. Project
    let proj_file = project_patterns_dir(project_root).join(format!("{name}.yaml"));
    if proj_file.is_file() {
        let pat = load_pattern_from_file(&proj_file)?;
        return Ok((pat, PatternSource::Project, Some(proj_file)));
    }

    // 2. Global
    let glob_file = global_patterns_dir_with(global_dir_override).join(format!("{name}.yaml"));
    if glob_file.is_file() {
        let pat = load_pattern_from_file(&glob_file)?;
        return Ok((pat, PatternSource::Global, Some(glob_file)));
    }

    // 3. Builtin
    if let Some(yaml) = get_builtin_template(name) {
        let pat: Pattern = serde_yaml_ng::from_str(yaml)
            .map_err(|e| PatternError::Serialization(e.to_string()))?;
        return Ok((pat, PatternSource::Builtin, None));
    }

    Err(PatternError::NotFound(name.to_string()))
}

pub fn save_pattern_to_dir(pattern: &Pattern, dir: &Path) -> Result<PathBuf, PatternError> {
    validate_pattern_name(&pattern.name)?;
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!("{}.yaml", pattern.name));
    let yaml_str = serde_yaml_ng::to_string(pattern)
        .map_err(|e| PatternError::Serialization(e.to_string()))?;

    let mut tmp = target.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(&tmp)?.write_all(yaml_str.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

pub fn save_pattern(
    pattern: &Pattern,
    is_global: bool,
    project_root: &Path,
    global_dir_override: Option<&Path>,
) -> Result<PathBuf, PatternError> {
    let dir = if is_global {
        global_patterns_dir_with(global_dir_override)
    } else {
        project_patterns_dir(project_root)
    };
    save_pattern_to_dir(pattern, &dir)
}

pub fn delete_pattern(
    name: &str,
    project_root: &Path,
    global_dir_override: Option<&Path>,
) -> Result<PatternSource, PatternError> {
    validate_pattern_name(name)?;
    let proj_file = project_patterns_dir(project_root).join(format!("{name}.yaml"));
    if proj_file.exists() {
        std::fs::remove_file(proj_file)?;
        return Ok(PatternSource::Project);
    }
    let glob_file = global_patterns_dir_with(global_dir_override).join(format!("{name}.yaml"));
    if glob_file.exists() {
        std::fs::remove_file(glob_file)?;
        return Ok(PatternSource::Global);
    }
    if is_builtin_template(name) {
        return Err(PatternError::CannotDeleteBuiltin(name.to_string()));
    }
    Err(PatternError::NotFound(name.to_string()))
}

pub fn duplicate_pattern(
    new_name: &str,
    from_name: &str,
    is_global: bool,
    project_root: &Path,
    global_dir_override: Option<&Path>,
) -> Result<(Pattern, PathBuf), PatternError> {
    validate_pattern_name(new_name)?;
    let (mut pattern, _source, _path) = load_pattern(from_name, project_root, global_dir_override)?;
    pattern.name = new_name.to_string();
    let path = save_pattern(&pattern, is_global, project_root, global_dir_override)?;
    Ok((pattern, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_dir(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "shepherd-test-{name}-{}-{nonce}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn test_builtin_templates_serde_roundtrip() {
        for tmpl in EMBEDDED_TEMPLATES {
            let pattern: Pattern =
                serde_yaml_ng::from_str(tmpl.yaml).expect("deserialize template");
            assert_eq!(pattern.name, tmpl.name);
            let serialized = serde_yaml_ng::to_string(&pattern).expect("serialize pattern");
            let roundtripped: Pattern =
                serde_yaml_ng::from_str(&serialized).expect("roundtrip deserialize");
            assert_eq!(pattern, roundtripped);
        }
    }

    #[test]
    fn test_builtin_templates_validate_without_errors() {
        let config = Config::default();
        for tmpl in EMBEDDED_TEMPLATES {
            let pattern: Pattern =
                serde_yaml_ng::from_str(tmpl.yaml).expect("deserialize template");
            let problems = validate(&pattern, &config);
            let errors: Vec<_> = problems.iter().filter(|p| p.is_error()).collect();
            assert!(
                errors.is_empty(),
                "template '{}' has validation errors: {:?}",
                tmpl.name,
                errors
            );
        }
    }

    #[test]
    fn test_validate_invalid_pattern_name_and_agent_id() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_ORCHESTRATOR_WORKER).unwrap();
        pattern.name = "bad name with spaces".into();
        pattern.agents[0].id = "invalid id!".into();

        let problems = validate(&pattern, &config);
        assert!(problems.iter().any(|p| p.path == "name" && p.is_error()));
        assert!(problems
            .iter()
            .any(|p| p.path == "agents[0].id" && p.is_error()));
    }

    #[test]
    fn test_validate_unknown_and_orphan_topics() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_ORCHESTRATOR_WORKER).unwrap();
        pattern.agents[0].publishes = vec!["nonexistent-topic".into()];
        pattern.topics.push(Topic {
            name: "orphan-topic".into(),
            partitions: 1,
            key: None,
            filter: None,
        });

        let problems = validate(&pattern, &config);
        // Error for unknown topic published
        assert!(problems.iter().any(|p| p.path == "agents[0].publishes[0]"
            && p.severity == Severity::Error
            && p.message.contains("nonexistent-topic")));
        // Warning for orphan topic
        assert!(problems.iter().any(|p| p.path.contains("topics")
            && p.severity == Severity::Warning
            && p.message.contains("orphan-topic")));
    }

    #[test]
    fn test_validate_unknown_role_and_uninstalled_vendor() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_ORCHESTRATOR_WORKER).unwrap();
        pattern.agents[0].role = "nonexistent-role".into();
        pattern.agents[1].vendor = Some("fake-vendor".into());

        let problems = validate(&pattern, &config);
        assert!(problems.iter().any(|p| p.path == "agents[0].role"
            && p.is_error()
            && p.message.contains("nonexistent-role")));
        assert!(problems.iter().any(|p| p.path == "agents[1].vendor"
            && p.severity == Severity::Warning
            && p.message.contains("fake-vendor")));
    }

    #[test]
    fn test_validate_limits() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_ORCHESTRATOR_WORKER).unwrap();
        pattern.agents[0].count = 0;
        pattern.agents[1].count = 100;
        pattern.topics[0].partitions = 0;
        pattern.topics[1].partitions = 500;
        pattern.agents[0].prompt = Some("a".repeat(20000));

        let problems = validate(&pattern, &config);
        assert!(problems
            .iter()
            .any(|p| p.path == "agents[0].count" && p.is_error()));
        assert!(problems
            .iter()
            .any(|p| p.path == "agents[1].count" && p.is_error()));
        assert!(problems
            .iter()
            .any(|p| p.path == "topics[0].partitions" && p.is_error()));
        assert!(problems
            .iter()
            .any(|p| p.path == "topics[1].partitions" && p.is_error()));
        assert!(problems
            .iter()
            .any(|p| p.path == "agents[0].prompt" && p.is_error()));
    }

    #[test]
    fn test_validate_orchestrator_worker_missing_worker_group() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_ORCHESTRATOR_WORKER).unwrap();
        pattern.agents[1].group = None;

        let problems = validate(&pattern, &config);
        assert!(problems
            .iter()
            .any(|p| p.is_error() && p.message.contains("worker")));
    }

    #[test]
    fn test_validate_hierarchical_gap_and_cycles() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_HIERARCHICAL).unwrap();
        // Remove level 1 manager so there's a gap between 0 and 2
        pattern.agents.retain(|a| a.level != Some(1));

        let problems = validate(&pattern, &config);
        assert!(problems
            .iter()
            .any(|p| p.is_error() && p.message.contains("gap")));
    }

    #[test]
    fn test_validate_blackboard_minimum_agents() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_BLACKBOARD).unwrap();
        pattern.agents.truncate(1);
        pattern.agents[0].count = 1;

        let problems = validate(&pattern, &config);
        assert!(problems
            .iter()
            .any(|p| p.is_error() && p.message.contains("at least 2")));
    }

    #[test]
    fn test_validate_market_missing_topic_and_solvers() {
        let config = Config::default();
        let mut pattern: Pattern = serde_yaml_ng::from_str(TEMPLATE_MARKET).unwrap();
        pattern.topics.retain(|t| t.name != "transaction");
        pattern.agents.retain(|a| a.id != "solver");

        let problems = validate(&pattern, &config);
        assert!(problems
            .iter()
            .any(|p| p.is_error() && p.message.contains("transaction")));
        assert!(problems
            .iter()
            .any(|p| p.is_error() && p.message.contains("solver")));
    }

    #[test]
    fn test_storage_project_and_global_layering_and_shadowing() {
        let proj_root = make_test_dir("pat-proj");
        let glob_dir = make_test_dir("pat-glob");

        // 1. Initial listing: only builtins
        let list1 = list_patterns(&proj_root, Some(&glob_dir)).unwrap();
        assert_eq!(list1.len(), 4);
        assert!(list1
            .iter()
            .all(|s| s.source == PatternSource::Builtin && !s.shadowed));

        // 2. Duplicate a pattern to global
        let (glob_pat, glob_path) = duplicate_pattern(
            "orchestrator-worker",
            "orchestrator-worker",
            true,
            &proj_root,
            Some(&glob_dir),
        )
        .unwrap();
        assert!(glob_path.exists());
        assert_eq!(glob_pat.name, "orchestrator-worker");

        // Now list should show global version unshadowed, and builtin version shadowed
        let list2 = list_patterns(&proj_root, Some(&glob_dir)).unwrap();
        let ow_entries: Vec<_> = list2
            .iter()
            .filter(|s| s.name == "orchestrator-worker")
            .collect();
        assert_eq!(ow_entries.len(), 2);
        let glob_entry = ow_entries
            .iter()
            .find(|s| s.source == PatternSource::Global)
            .unwrap();
        let built_entry = ow_entries
            .iter()
            .find(|s| s.source == PatternSource::Builtin)
            .unwrap();
        assert!(!glob_entry.shadowed);
        assert!(built_entry.shadowed);

        // 3. Save to project
        let (proj_pat, proj_path) = duplicate_pattern(
            "orchestrator-worker",
            "orchestrator-worker",
            false,
            &proj_root,
            Some(&glob_dir),
        )
        .unwrap();
        assert!(proj_path.exists());
        assert_eq!(proj_pat.name, "orchestrator-worker");

        // Now project is unshadowed, global is shadowed, builtin is shadowed
        let list3 = list_patterns(&proj_root, Some(&glob_dir)).unwrap();
        let ow_entries3: Vec<_> = list3
            .iter()
            .filter(|s| s.name == "orchestrator-worker")
            .collect();
        assert_eq!(ow_entries3.len(), 3);
        let proj_entry = ow_entries3
            .iter()
            .find(|s| s.source == PatternSource::Project)
            .unwrap();
        let glob_entry = ow_entries3
            .iter()
            .find(|s| s.source == PatternSource::Global)
            .unwrap();
        let built_entry = ow_entries3
            .iter()
            .find(|s| s.source == PatternSource::Builtin)
            .unwrap();
        assert!(!proj_entry.shadowed);
        assert!(glob_entry.shadowed);
        assert!(built_entry.shadowed);

        // 4. Load resolution priority
        let (loaded, src, path) =
            load_pattern("orchestrator-worker", &proj_root, Some(&glob_dir)).unwrap();
        assert_eq!(src, PatternSource::Project);
        assert_eq!(path.unwrap(), proj_path);
        assert_eq!(loaded.name, "orchestrator-worker");

        // 5. Delete project pattern -> global becomes active
        let deleted_src =
            delete_pattern("orchestrator-worker", &proj_root, Some(&glob_dir)).unwrap();
        assert_eq!(deleted_src, PatternSource::Project);
        let (loaded_after, src_after, _) =
            load_pattern("orchestrator-worker", &proj_root, Some(&glob_dir)).unwrap();
        assert_eq!(src_after, PatternSource::Global);
        assert_eq!(loaded_after.name, "orchestrator-worker");

        // 6. Delete global pattern -> builtin becomes active
        let deleted_src2 =
            delete_pattern("orchestrator-worker", &proj_root, Some(&glob_dir)).unwrap();
        assert_eq!(deleted_src2, PatternSource::Global);
        let (loaded_built, src_built, _) =
            load_pattern("orchestrator-worker", &proj_root, Some(&glob_dir)).unwrap();
        assert_eq!(src_built, PatternSource::Builtin);
        assert_eq!(loaded_built.name, "orchestrator-worker");

        // 7. Cannot delete builtin pattern
        let err = delete_pattern("orchestrator-worker", &proj_root, Some(&glob_dir)).unwrap_err();
        match err {
            PatternError::CannotDeleteBuiltin(_) => {}
            other => panic!("expected CannotDeleteBuiltin, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&proj_root);
        let _ = std::fs::remove_dir_all(&glob_dir);
    }
}
