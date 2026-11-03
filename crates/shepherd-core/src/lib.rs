//! Rust domain core for Shepherd.
//!
//! This crate is intentionally independent of terminal, filesystem, and AI vendor
//! integrations. Those are adapters around the same workflow rules.

pub mod herdr;
pub mod terminals;
pub mod bus;
pub mod cli;
pub mod config;
pub mod service;
pub mod skills;
pub mod spawn;
pub mod mcp;
pub mod workflow;

pub use cli::{run_cli, run_cli_in_root, run_cli_with_terminals};

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Subtask {
    #[serde(rename = "n")]
    pub number: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "depends")]
    pub depends_on: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify: Vec<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub coder: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reviewer: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub round: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pr: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScheduleRound {
    pub ready: Vec<u32>,
    pub dependency_failures: Vec<(u32, u32)>,
    pub stalled: bool,
}

/// Choose the next dependency-safe subtasks, matching the Go workflow engine's runAll policy.
/// `running` contains subtask numbers already assigned to workers.
pub fn schedule_subtasks(subtasks: &mut [Subtask], running: &[u32], max_parallel: usize) -> ScheduleRound {
    let states: HashMap<u32, &str> = subtasks.iter().map(|s| (s.number, s.state.as_str())).collect();
    let running: HashSet<u32> = running.iter().copied().collect();
    let mut round = ScheduleRound::default();
    let mut pending_left = false;

    for subtask in subtasks.iter() {
        if subtask.state != "pending" || running.contains(&subtask.number) { continue; }
        pending_left = true;
        let mut failed_dependency = None;
        let mut waiting = false;
        for dependency in &subtask.depends_on {
            match states.get(dependency).copied().unwrap_or("") {
                "pr" | "done" => {}
                "needs-human" | "failed" => failed_dependency = Some(*dependency),
                _ => waiting = true,
            }
        }
        if let Some(dependency) = failed_dependency {
            round.dependency_failures.push((subtask.number, dependency));
        } else if !waiting {
            round.ready.push(subtask.number);
        }
    }

    for (number, dependency) in &round.dependency_failures {
        if let Some(subtask) = subtasks.iter_mut().find(|s| s.number == *number) {
            subtask.state = "failed".into();
            subtask.note = Some(format!("dependency {dependency} did not finish"));
        }
    }

    let capacity = max_parallel.max(1).saturating_sub(running.len());
    round.ready.truncate(capacity);
    if running.is_empty() && round.ready.is_empty() && pending_left {
        for subtask in subtasks.iter_mut().filter(|s| s.state == "pending") {
            subtask.state = "failed".into();
            subtask.note = Some("dependencies cannot make progress".into());
        }
        round.stalled = true;
    }
    round
}

impl Subtask {
    fn pending(number: u32, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            number,
            title: title.into(),
            body: body.into(),
            depends_on: Vec::new(),
            verify: Vec::new(),
            state: "pending".into(),
            note: None,
            coder: String::new(),
            reviewer: String::new(),
            round: 0,
            branch: String::new(),
            pr: String::new(),
        }
    }
}

/// Derive the task-level state shown to the user from its subtasks.
pub fn aggregate_task_state(subtasks: &[Subtask], previous: &str) -> String {
    if subtasks.is_empty() { return previous.to_owned(); }
    let has = |states: &[&str]| subtasks.iter().any(|s| states.contains(&s.state.as_str()));
    if has(&["coding", "fixing"]) { return "coding".into(); }
    if has(&["reviewing", "checks"]) { return "reviewing".into(); }
    if has(&["awaiting-approval"]) { return "pr".into(); }
    if has(&["pending"]) { return "coding".into(); }
    if subtasks.iter().all(|s| matches!(s.state.as_str(), "pr" | "done")) { return "done".into(); }
    previous.to_owned()
}

/// Prepare a persisted workflow for resume after daemon interruption.
/// Completed PRs remain complete; every other subtask returns to pending.
pub fn prepare_resume(subtasks: &mut [Subtask]) -> usize {
    let mut kept = 0;
    for subtask in subtasks {
        if matches!(subtask.state.as_str(), "pr" | "done") {
            kept += 1;
        } else {
            subtask.state = "pending".into();
            subtask.note = Some("resumed after an interruption".into());
        }
    }
    kept
}

/// Reset an interrupted persisted workflow for a future run while retaining completed work.
/// The returned count is the number of subtasks already completed as PRs or marked done.
pub fn prepare_task_resume(status: &mut TaskStatus) -> usize {
    status.waiting.clear();
    let kept = prepare_resume(&mut status.subtasks);
    status.state = aggregate_task_state(&status.subtasks, "coding");
    status.updated = now_rfc3339();
    kept
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Criterion {
    pub met: bool,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verdict {
    pub approve: bool,
    pub found: bool,
    pub criteria: Vec<Criterion>,
    pub downgraded: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    DuplicateSubtask(u32),
    DependencyCycle(u32),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSubtask(n) => write!(f, "duplicate subtask number {n}"),
            Self::DependencyCycle(n) => write!(f, "dependency cycle includes subtask {n}"),
        }
    }
}

impl std::error::Error for PlanError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IssueRef {
    pub number: u32,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
}

/// On-disk task state, kept compatible with Go's `.shepherd/tasks/*/status.json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskStatus {
    pub id: u32,
    pub slug: String,
    pub title: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueRef>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pr: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<Subtask>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub waiting: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// RFC3339 strings preserve Go's `time.Time` JSON representation.
    pub created: String,
    pub updated: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillAdjust {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

/// Shepherd's identity and workflow metadata for an agent. Herdr remains the owner of its terminal process.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub seq: u64,
    pub role: String,
    pub vendor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
    #[serde(default)]
    pub depth: u32,
    pub root: String,
    #[serde(default)]
    pub mcp: bool,
    #[serde(default)]
    pub skills: SkillAdjust,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tab: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pane: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub worktree: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notice: String,
    #[serde(default)]
    pub has_result: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    pub started: String,
    pub updated: String,
}

impl Agent {
    pub fn terminal(&self) -> bool { matches!(self.state.as_str(), "done" | "exited" | "killed" | "failed") }
    pub fn gone(&self) -> bool { matches!(self.state.as_str(), "exited" | "killed" | "failed") }
    pub fn public(&self) -> Self { let mut agent = self.clone(); agent.token.clear(); agent }
}

/// On-disk `agents.json`; older Go versions wrote a bare JSON array, which is still accepted.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentRegistryFile {
    #[serde(default)]
    pub seq: u64,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub next: HashMap<String, u64>,
    #[serde(default)]
    pub agents: Vec<Agent>,
}

impl AgentRegistryFile {
    /// Allocate role-local IDs and a global sequence. The token must be generated by a CSPRNG.
    pub fn create(&mut self, mut agent: Agent, token: String, now: &str) -> Agent {
        self.recover_counters();
        self.seq += 1;
        let mut number = self.next.get(&agent.role).copied().unwrap_or(0) + 1;
        loop {
            let candidate = format!("{}-{number}", agent.role);
            if !self.agents.iter().any(|existing| existing.id == candidate) {
                agent.id = candidate;
                break;
            }
            number += 1;
        }
        self.next.insert(agent.role.clone(), number);
        agent.seq = self.seq;
        agent.token = token;
        agent.state = "starting".into();
        agent.started = now.into();
        agent.updated = now.into();
        self.agents.push(agent.clone());
        agent
    }

    pub fn create_secure(&mut self, agent: Agent, now: &str) -> Result<Agent, getrandom::Error> {
        Ok(self.create(agent, new_agent_token()?, now))
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.recover_counters();
        let old_len = self.agents.len();
        self.agents.retain(|agent| agent.id != id);
        old_len != self.agents.len()
    }

    /// Store the agent's final MCP result and mark it done, matching Go's SubmitResult transition.
    pub fn submit_result(&mut self, id: &str, summary: &str, now: &str) -> Result<(), String> {
        if summary.trim().is_empty() { return Err("result summary is empty".into()); }
        let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == id) else {
            return Err(format!("unknown agent {id:?}"));
        };
        agent.result = summary.trim().to_owned();
        agent.has_result = true;
        agent.state = "done".into();
        agent.updated = now.to_owned();
        Ok(())
    }

    pub fn result(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    pub fn clear_result(&mut self, id: &str, now: &str) -> Result<(), String> {
        let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == id) else { return Err(format!("unknown agent {id:?}")); };
        agent.result.clear();
        agent.has_result = false;
        agent.error.clear();
        agent.state = "working".into();
        agent.updated = now.to_owned();
        Ok(())
    }

    pub fn live(&self) -> Vec<&Agent> { self.agents.iter().filter(|agent| !agent.terminal()).collect() }

    pub fn by_token(&self, token: &str) -> Option<&Agent> {
        if token.is_empty() { return None; }
        self.agents.iter().find(|agent| agent.token == token)
    }
}

pub fn new_agent_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes { use std::fmt::Write; let _ = write!(token, "{byte:02x}"); }
    Ok(token)
}

pub fn load_agent_registry(path: impl AsRef<Path>) -> Result<AgentRegistryFile, Box<dyn std::error::Error + Send + Sync>> {
    let path = path.as_ref();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(AgentRegistryFile::default()),
        Err(error) => return Err(error.into()),
    };
    let mut registry = if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'[') {
        AgentRegistryFile { agents: serde_json::from_slice(&bytes)?, ..AgentRegistryFile::default() }
    } else {
        serde_json::from_slice(&bytes)?
    };
    registry.recover_counters();
    Ok(registry)
}

impl AgentRegistryFile {
    fn recover_counters(&mut self) {
        for agent in &self.agents {
            self.seq = self.seq.max(agent.seq);
            if let Some(number) = agent.id.strip_prefix(&format!("{}-", agent.role)).and_then(|n| n.parse::<u64>().ok()) {
                self.next.entry(agent.role.clone()).and_modify(|n| *n = (*n).max(number)).or_insert(number);
            }
        }
    }
}

pub fn save_agent_registry(path: impl AsRef<Path>, registry: &AgentRegistryFile) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut registry = registry.clone();
    registry.recover_counters();
    registry.agents.sort_by_key(|agent| agent.seq);
    let mut bytes = serde_json::to_vec_pretty(&registry)?;
    bytes.push(b'\n');
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(&tmp)?.write_all(&bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn is_zero(value: &u32) -> bool { *value == 0 }

/// Read a task status JSON file produced by either the Go or Rust implementation.
pub fn load_task_status(path: impl AsRef<Path>) -> Result<TaskStatus, Box<dyn std::error::Error + Send + Sync>> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Atomically write task status using the same temp-file-and-rename strategy as Go.
pub fn save_task_status(path: impl AsRef<Path>, status: &TaskStatus) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = path.as_ref();
    let mut bytes = serde_json::to_vec_pretty(status)?;
    bytes.push(b'\n');
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

pub fn touch_task(task: &CreatedTask) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut status = task.status.clone();
    status.updated = now_rfc3339();
    save_task_status(task.directory.join("status.json"), &status)
}

pub struct NewTask {
    pub id: u32,
    pub title: String,
    pub description: String,
    pub criteria: Vec<String>,
    pub issue: Option<IssueRef>,
    pub issue_markdown: Option<String>,
    pub links: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedTask {
    pub status: TaskStatus,
    pub directory: std::path::PathBuf,
}

pub fn list_tasks(root: impl AsRef<Path>, archived: bool) -> Result<Vec<CreatedTask>, Box<dyn std::error::Error + Send + Sync>> {
    let mut directory = root.as_ref().join(".shepherd").join("tasks");
    if archived { directory.push("_archive"); }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) { continue; }
        if entry.file_name().to_string_lossy().starts_with('_') { continue; }
        let path = entry.path();
        if let Ok(status) = load_task_status(path.join("status.json")) {
            tasks.push(CreatedTask { status, directory: path });
        }
    }
    tasks.sort_by_key(|task| task.status.id);
    Ok(tasks)
}

pub fn find_task(root: impl AsRef<Path>, reference: &str) -> Result<CreatedTask, Box<dyn std::error::Error + Send + Sync>> {
    let tasks = list_tasks(root, false)?;
    let mut prefix_matches = Vec::new();
    for task in tasks {
        let name = format!("{}-{}", task.status.id, task.status.slug);
        if task.status.id.to_string() == reference || name == reference { return Ok(task); }
        if name.starts_with(reference) { prefix_matches.push(task); }
    }
    match prefix_matches.len() {
        1 => Ok(prefix_matches.remove(0)),
        0 => Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("no task {reference:?}")).into()),
        n => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("task {reference:?} is ambiguous ({n} matches)")).into()),
    }
}

const TASK_SECTIONS: [&str; 5] = ["issue", "spec", "acceptance", "links", "plan"];

pub fn task_section_path(task: &CreatedTask, section: &str) -> Result<std::path::PathBuf, std::io::Error> {
    if !TASK_SECTIONS.contains(&section) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
            format!("unknown section {section:?} (have: {})", TASK_SECTIONS.join(", "))));
    }
    Ok(task.directory.join(format!("{section}.md")))
}

pub fn read_task_section(task: &CreatedTask, section: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let path = task_section_path(task, section)?;
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn write_task_section(task: &CreatedTask, section: &str, content: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = task_section_path(task, section)?;
    atomic_write(&path, content.as_bytes())?;
    Ok(())
}

pub fn task_context(task: &CreatedTask, limit_bytes: usize) -> String {
    let mut full = format!("# Task {}-{}\n\n", task.status.id, task.status.slug);
    let mut index = format!("# Task {}-{}\n\nRead these files (or call get_task_context(section)):\n\n", task.status.id, task.status.slug);
    for section in TASK_SECTIONS {
        let Ok(content) = read_task_section(task, section) else { continue };
        if content.trim().is_empty() { continue; }
        full.push_str(content.trim());
        full.push_str("\n\n");
        if let Ok(path) = task_section_path(task, section) {
            index.push_str(&format!("- {section} ({} bytes): {}\n", content.len(), path.display()));
        }
    }
    index.push_str(&format!("- attachments: {}\n", task.directory.join("attachments").display()));
    if limit_bytes > 0 && full.len() > limit_bytes { index } else { full }
}

pub fn archive_task(root: impl AsRef<Path>, reference: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let task = find_task(root.as_ref(), reference)?;
    let archive_dir = root.as_ref().join(".shepherd").join("tasks").join("_archive");
    std::fs::create_dir_all(&archive_dir)?;
    let destination = archive_dir.join(task.directory.file_name().unwrap_or_default());
    if destination.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", destination.display())).into());
    }
    std::fs::rename(task.directory, &destination)?;
    Ok(destination)
}

pub fn task_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for ch in title.nfd().flat_map(char::to_lowercase) {
        if is_combining_mark(ch) { continue; }
        if ch.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() { slug.push('-'); }
            slug.push(ch);
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.len() > 40 { slug.truncate(40); }
    while slug.ends_with('-') { slug.pop(); }
    if slug.is_empty() { "task".into() } else { slug }
}

pub fn acceptance_markdown(criteria: &[String]) -> String {
    let mut output = String::from("# Acceptance criteria\n\n");
    let mut count = 0;
    for criterion in criteria.iter().map(|c| c.trim()).filter(|c| !c.is_empty()) {
        output.push_str("- [ ] ");
        output.push_str(criterion);
        output.push('\n');
        count += 1;
    }
    if count == 0 {
        output.push_str("- [ ] (add checkable criteria; the reviewer verifies each one)\n");
    }
    output
}

/// Create a task directory and files using the same layout as Go's task.Create.
pub fn create_task(root: impl AsRef<Path>, mut input: NewTask) -> Result<CreatedTask, Box<dyn std::error::Error + Send + Sync>> {
    if input.title.trim().is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "a task needs a title").into());
    }
    let tasks_dir = root.as_ref().join(".shepherd").join("tasks");
    if input.id == 0 { input.id = next_task_id(&tasks_dir); }
    if let Some(existing) = list_tasks(root.as_ref(), false)?.into_iter().find(|t| t.status.id == input.id) {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists,
            format!("task {} already exists ({})", input.id, existing.directory.display())).into());
    }
    let slug = task_slug(&input.title);
    let directory = tasks_dir.join(format!("{}-{}", input.id, slug));
    std::fs::create_dir_all(&tasks_dir)?;
    std::fs::create_dir(&directory)?;
    std::fs::create_dir(directory.join("attachments"))?;

    let issue = input.issue_markdown.unwrap_or_else(|| format!("# {}\n\n{}\n", input.title, input.description.trim()));
    let mut links = String::from("# Links\n\n");
    if let Some(issue_ref) = &input.issue { links.push_str(&format!("- Issue: {}\n", issue_ref.url)); }
    for link in input.links { links.push_str(&format!("- {}\n", link)); }
    for (name, content) in [
        ("issue.md", issue),
        ("spec.md", "# Spec\n\nDesign decisions, constraints, interfaces. Agents read this before coding.\n".to_owned()),
        ("acceptance.md", acceptance_markdown(&input.criteria)),
        ("links.md", links),
    ] {
        atomic_write(&directory.join(name), content.as_bytes())?;
    }

    let now = now_rfc3339();
    let status = TaskStatus {
        id: input.id,
        slug,
        title: input.title,
        state: "new".into(),
        issue: input.issue,
        branch: String::new(),
        pr: 0,
        subtasks: Vec::new(),
        waiting: String::new(),
        note: String::new(),
        agents: Vec::new(),
        created: now.clone(),
        updated: now,
    };
    save_task_status(directory.join("status.json"), &status)?;
    Ok(CreatedTask { status, directory })
}

fn next_task_id(tasks_dir: &Path) -> u32 {
    let mut max_id = 0;
    for dir in [tasks_dir.to_path_buf(), tasks_dir.join("_archive")] {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            if let Some((id, _)) = entry.file_name().to_str().and_then(parse_task_name) {
                max_id = max_id.max(id);
            }
        }
    }
    max_id + 1
}

fn parse_task_name(name: &str) -> Option<(u32, &str)> {
    let (number, slug) = name.split_once('-')?;
    if number.is_empty() || slug.is_empty() { return None; }
    Some((number.parse().ok()?, slug))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (seconds / 86_400) as i64;
    let day_seconds = seconds % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = day_seconds / 3600;
    let minute = (day_seconds % 3600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Parse the plan format used by Shepherd's plan-tasks skill.
/// A free-form plan becomes one subtask, matching the Go implementation.
pub fn parse_plan(plan: &str, fallback_title: &str) -> Vec<Subtask> {
    let lines: Vec<&str> = plan.lines().collect();
    let heads: Vec<(usize, u32, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| parse_subtask_heading(line).map(|(n, title)| (i, n, title)))
        .collect();

    if heads.is_empty() {
        return vec![Subtask::pending(1, fallback_title, plan.trim())];
    }

    let mut out = Vec::with_capacity(heads.len());
    for (index, (start, number, title)) in heads.iter().enumerate() {
        let end = heads.get(index + 1).map_or(lines.len(), |h| h.0);
        let body = lines[*start + 1..end].join("\n").trim().to_owned();
        let mut subtask = Subtask::pending(*number, title, body.clone());
        for line in body.lines() {
            if let Some(value) = field_value(line, "verify") {
                let command = strip_backticks(value.trim());
                if !command.is_empty() {
                    subtask.verify.push(command.to_owned());
                }
            }
            if let Some(value) = field_value(line, "depends on") {
                if !value.to_ascii_lowercase().contains("none") {
                    for number in digit_runs(value) {
                        if number != subtask.number {
                            subtask.depends_on.push(number);
                        }
                    }
                }
            }
        }
        out.push(subtask);
    }

    let have: HashSet<u32> = out.iter().map(|s| s.number).collect();
    for subtask in &mut out {
        subtask.depends_on.retain(|n| have.contains(n));
    }
    out
}

fn parse_subtask_heading(line: &str) -> Option<(u32, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    if !(2..=4).contains(&hashes) {
        return None;
    }
    let mut rest = trimmed[hashes..].trim_start();
    if let Some(after) = rest.strip_prefix("Subtask ") {
        rest = after;
    }
    let digit_count = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return None;
    }
    let number = rest[..digit_count].parse().ok()?;
    let rest = rest[digit_count..].trim_start();
    if !matches!(rest.chars().next()?, '.' | ')' | ':') { return None; }
    let rest = rest[1..].trim_start();
    let title = rest.trim_end();
    if title.is_empty() { None } else { Some((number, title.to_owned())) }
}

fn field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let line = line.trim().trim_start_matches(|c| c == '-' || c == '*').trim();
    let line = line.trim_matches('*').trim();
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(field).then_some(value.trim())
}

fn strip_backticks(value: &str) -> &str {
    value.strip_prefix('`').and_then(|s| s.strip_suffix('`')).unwrap_or(value).trim()
}

fn digit_runs(value: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    for ch in value.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(n) = current.parse() { numbers.push(n); }
            current.clear();
        }
    }
    numbers
}

/// Reject duplicate identifiers and dependency cycles before a plan is run.
pub fn validate_plan(subtasks: &[Subtask]) -> Result<(), PlanError> {
    let mut by_id = HashMap::with_capacity(subtasks.len());
    for subtask in subtasks {
        if by_id.insert(subtask.number, subtask).is_some() {
            return Err(PlanError::DuplicateSubtask(subtask.number));
        }
    }
    fn visit(n: u32, by_id: &HashMap<u32, &Subtask>, marks: &mut HashMap<u32, u8>) -> Result<(), PlanError> {
        match marks.get(&n).copied().unwrap_or(0) {
            1 => return Err(PlanError::DependencyCycle(n)),
            2 => return Ok(()),
            _ => {}
        }
        marks.insert(n, 1);
        if let Some(subtask) = by_id.get(&n) {
            for dep in &subtask.depends_on {
                if by_id.contains_key(dep) { visit(*dep, by_id, marks)?; }
            }
        }
        marks.insert(n, 2);
        Ok(())
    }
    let mut marks = HashMap::new();
    for n in by_id.keys() { visit(*n, &by_id, &mut marks)?; }
    Ok(())
}

/// Count acceptance checklist items, excluding the empty-task template line.
pub fn count_criteria(acceptance: &str) -> usize {
    acceptance.lines().filter_map(parse_checklist).filter(|(_, text)| !text.starts_with("(add checkable criteria")).count()
}

/// Parse reviewer output and downgrade APPROVE unless every requested criterion is answered and met.
pub fn parse_review(text: &str, want_criteria: usize) -> Verdict {
    let mut verdict = Verdict { approve: false, found: false, criteria: Vec::new(), downgraded: None, text: text.trim().to_owned() };
    let Some(value) = text.lines().find_map(|line| field_value(line, "verdict")) else { return verdict };
    let value = value.trim_matches('*').trim().to_ascii_lowercase();
    if !(value.starts_with("approve") || value.starts_with("request_changes") || value.starts_with("request changes")) { return verdict; }
    verdict.found = true;
    verdict.approve = value.starts_with("approve");

    let upper = text.to_ascii_uppercase();
    let section_start = upper.find("CRITERIA").unwrap_or(0);
    let mut section_end = text.len();
    for marker in ["COMMENTS", "SUMMARY"] {
        if let Some(pos) = upper[section_start..].find(marker) {
            if pos > 0 { section_end = section_end.min(section_start + pos); }
        }
    }
    for line in text[section_start..section_end].lines() {
        if let Some((met, text)) = parse_checklist(line) {
            verdict.criteria.push(Criterion { met, text: text.to_owned() });
        }
    }
    if verdict.approve && want_criteria > 0 {
        let met = verdict.criteria.iter().filter(|c| c.met).count();
        if verdict.criteria.len() < want_criteria {
            verdict.approve = false;
            verdict.downgraded = Some(format!("the review answered {} of {} acceptance criteria", verdict.criteria.len(), want_criteria));
        } else if met < verdict.criteria.len() {
            verdict.approve = false;
            verdict.downgraded = Some("approved with unmet criteria".into());
        }
    }
    verdict
}

fn parse_checklist(line: &str) -> Option<(bool, &str)> {
    let line = line.trim_start();
    let line = line.strip_prefix('-').or_else(|| line.strip_prefix('*'))?.trim_start();
    let line = line.strip_prefix('[')?;
    let (mark, rest) = line.split_once(']')?;
    if mark.len() != 1 || !matches!(mark.as_bytes()[0], b' ' | b'x' | b'X') { return None; }
    let text = rest.trim();
    (!text.is_empty()).then_some((mark != " ", text))
}

/// JSON element returned by `shepherd task list [--archived] --json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskListEntryJson {
    #[serde(flatten)]
    pub status: TaskStatus,
    pub directory: String,
}

/// JSON object returned by `shepherd task show <id> --json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskDetailJson {
    #[serde(flatten)]
    pub status: TaskStatus,
    pub directory: String,
    pub sections: std::collections::BTreeMap<String, String>,
    pub context: String,
}

/// JSON object returned by `shepherd task show <id> <section> --json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskSectionJson {
    pub id: u32,
    pub slug: String,
    pub section: String,
    pub path: String,
    pub content: String,
}

/// JSON element returned by `shepherd roles list --json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoleListEntryJson {
    pub role: String,
    pub configured_vendor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub configured_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub fallback: bool,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub worktree: bool,
    pub can_spawn: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

/// Serializes `registry.agents` as a public JSON array with `token` omitted from every entry.
pub fn agents_json(registry: &AgentRegistryFile) -> Result<String, serde_json::Error> {
    let public: Vec<Agent> = registry.agents.iter().map(Agent::public).collect();
    serde_json::to_string_pretty(&public)
}

/// Serializes a task list as a JSON array of `TaskListEntryJson`.
pub fn task_list_json(tasks: &[CreatedTask]) -> Result<String, serde_json::Error> {
    let entries: Vec<TaskListEntryJson> = tasks
        .iter()
        .map(|task| TaskListEntryJson {
            status: task.status.clone(),
            directory: task.directory.display().to_string(),
        })
        .collect();
    serde_json::to_string_pretty(&entries)
}

/// Serializes either a full task view (`section == None`) or a single task section (`section == Some(...)`) as JSON.
pub fn task_show_json(
    task: &CreatedTask,
    section: Option<&str>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(section_name) = section {
        let path = task_section_path(task, section_name)?;
        let content = read_task_section(task, section_name)?;
        let payload = TaskSectionJson {
            id: task.status.id,
            slug: task.status.slug.clone(),
            section: section_name.to_owned(),
            path: path.display().to_string(),
            content,
        };
        return Ok(serde_json::to_string_pretty(&payload)?);
    }
    let mut sections = std::collections::BTreeMap::new();
    for name in TASK_SECTIONS {
        if let Ok(path) = task_section_path(task, name) {
            if path.exists() {
                if let Ok(content) = read_task_section(task, name) {
                    sections.insert(name.to_owned(), content);
                }
            }
        }
    }
    let payload = TaskDetailJson {
        status: task.status.clone(),
        directory: task.directory.display().to_string(),
        sections,
        context: task_context(task, 0),
    };
    Ok(serde_json::to_string_pretty(&payload)?)
}

/// Serializes resolved role assignments as a JSON array of `RoleListEntryJson`.
pub fn roles_list_json(
    cfg: &config::Config,
    active_vendors: &[String],
) -> Result<String, serde_json::Error> {
    let mut role_names: Vec<_> = cfg.roles.keys().cloned().collect();
    role_names.sort();
    let mut entries = Vec::with_capacity(role_names.len());
    for name in role_names {
        let role_cfg = &cfg.roles[&name];
        match config::resolve_role(cfg, active_vendors, &name) {
            Ok(assigned) => entries.push(RoleListEntryJson {
                role: name,
                configured_vendor: role_cfg.vendor.clone(),
                configured_model: role_cfg.model.clone(),
                vendor: Some(assigned.vendor),
                model: Some(assigned.model),
                fallback: assigned.fallback,
                available: true,
                error: None,
                worktree: role_cfg.worktree,
                can_spawn: role_cfg.can_spawn,
                skills: role_cfg.skills.clone(),
            }),
            Err(err) => entries.push(RoleListEntryJson {
                role: name,
                configured_vendor: role_cfg.vendor.clone(),
                configured_model: role_cfg.model.clone(),
                vendor: None,
                model: None,
                fallback: false,
                available: false,
                error: Some(err.to_string()),
                worktree: role_cfg.worktree,
                can_spawn: role_cfg.can_spawn,
                skills: role_cfg.skills.clone(),
            }),
        }
    }
    serde_json::to_string_pretty(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_and_verify_commands() {
        let plan = "### 1. API\nFiles: src/api.rs\nVerify: `cargo test -p api`\n\n### Subtask 2: UI\nDepends on: 1\nVerify: cargo test";
        let parsed = parse_plan(plan, "fallback");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].title, "API");
        assert_eq!(parsed[0].verify, ["cargo test -p api"]);
        assert_eq!(parsed[1].depends_on, [1]);
        assert!(validate_plan(&parsed).is_ok());
    }

    #[test]
    fn detects_dependency_cycle_and_duplicate_ids() {
        let mut a = Subtask::pending(1, "a", "");
        let mut b = Subtask::pending(2, "b", "");
        a.depends_on = vec![2];
        b.depends_on = vec![1];
        assert!(matches!(validate_plan(&[a.clone(), b.clone()]), Err(PlanError::DependencyCycle(_))));
        assert_eq!(validate_plan(&[a.clone(), a]), Err(PlanError::DuplicateSubtask(1)));
    }

    #[test]
    fn requires_every_acceptance_criterion_for_approval() {
        let review = "VERDICT: APPROVE\n\nCRITERIA\n- [x] login works\n- [ ] refresh works\n\nSUMMARY\nLooks good";
        let verdict = parse_review(review, 2);
        assert!(!verdict.approve);
        assert_eq!(verdict.downgraded.as_deref(), Some("approved with unmet criteria"));
    }

    #[test]
    fn ignores_acceptance_template_placeholder() {
        assert_eq!(count_criteria("- [ ] (add checkable criteria; reviewer verifies)"), 0);
        assert_eq!(count_criteria("- [ ] first\n- [x] second"), 2);
    }

    #[test]
    fn aggregates_subtask_states_with_work_in_progress_priority() {
        let mut first = Subtask::pending(1, "first", "");
        let mut second = Subtask::pending(2, "second", "");
        first.state = "pr".into();
        second.state = "reviewing".into();
        assert_eq!(aggregate_task_state(&[first.clone(), second.clone()], "new"), "reviewing");
        second.state = "awaiting-approval".into();
        assert_eq!(aggregate_task_state(&[first.clone(), second], "new"), "pr");
        assert_eq!(aggregate_task_state(&[first], "new"), "done");
        assert_eq!(aggregate_task_state(&[], "planning"), "planning");
    }

    #[test]
    fn resume_keeps_completed_subtasks_and_resets_the_rest() {
        let mut completed = Subtask::pending(1, "done", "");
        completed.state = "pr".into();
        let mut interrupted = Subtask::pending(2, "interrupted", "");
        interrupted.state = "reviewing".into();
        let mut subs = vec![completed, interrupted];
        assert_eq!(prepare_resume(&mut subs), 1);
        assert_eq!(subs[0].state, "pr");
        assert_eq!(subs[1].state, "pending");
        assert_eq!(subs[1].note.as_deref(), Some("resumed after an interruption"));
    }

    #[test]
    fn resume_clears_stale_gate_and_updates_task_state() {
        let mut status: TaskStatus = serde_json::from_value(serde_json::json!({
            "id": 3, "slug": "plan", "title": "Plan", "state": "pr", "waiting": "pr:2",
            "created": "2026-11-03T10:00:00Z", "updated": "2026-11-03T10:00:00Z",
            "subtasks": [
                {"n": 1, "title": "done", "state": "pr", "pr": "https://example.test/pr/1"},
                {"n": 2, "title": "interrupted", "state": "awaiting-approval"}
            ]
        })).unwrap();
        assert_eq!(prepare_task_resume(&mut status), 1);
        assert!(status.waiting.is_empty());
        assert_eq!(status.state, "coding");
        assert_eq!(status.subtasks[0].state, "pr");
        assert_eq!(status.subtasks[1].state, "pending");
        assert_ne!(status.updated, "2026-11-03T10:00:00Z");
    }

    #[test]
    fn reads_and_writes_go_task_status_json() {
        let source = r#"{
            "id": 8, "slug": "add-login", "title": "Add login", "state": "coding",
            "issue": {"number": 42, "url": "https://example.test/issues/42"},
            "subtasks": [{"n": 1, "title": "API", "body": "Files: api.rs", "depends": [],
                          "verify": ["cargo test"], "state": "pending"}],
            "created": "2026-11-03T10:00:00Z", "updated": "2026-11-03T10:00:00Z"
        }"#;
        let status: TaskStatus = serde_json::from_str(source).unwrap();
        assert_eq!(status.id, 8);
        assert_eq!(status.subtasks[0].number, 1);
        assert_eq!(status.subtasks[0].verify, ["cargo test"]);

        let path = std::env::temp_dir().join(format!("shepherd-status-{}.json", std::process::id()));
        save_task_status(&path, &status).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert!(value["subtasks"][0].get("n").is_some());
        assert!(value["subtasks"][0].get("number").is_none());
        assert!(value["subtasks"][0].get("depends_on").is_none());
        let loaded = load_task_status(&path).unwrap();
        assert_eq!(loaded, status);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn creates_go_compatible_task_layout_and_unicode_slug() {
        assert_eq!(task_slug("Añadir OAuth: sesión"), "anadir-oauth-sesion");
        let root = std::env::temp_dir().join(format!("shepherd-create-task-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let task = create_task(&root, NewTask {
            id: 0,
            title: "Añadir OAuth: sesión".into(),
            description: "Support login".into(),
            criteria: vec!["login works".into(), "  ".into()],
            issue: Some(IssueRef { number: 5, url: "https://example.test/issues/5".into(), repo: String::new() }),
            issue_markdown: None,
            links: vec!["design doc".into()],
        }).unwrap();

        assert_eq!(task.status.id, 1);
        assert_eq!(task.directory.file_name().unwrap(), "1-anadir-oauth-sesion");
        assert!(task.directory.join("attachments").is_dir());
        assert!(task.directory.join("status.json").is_file());
        let acceptance = std::fs::read_to_string(task.directory.join("acceptance.md")).unwrap();
        assert_eq!(count_criteria(&acceptance), 1);
        let links = std::fs::read_to_string(task.directory.join("links.md")).unwrap();
        assert!(links.contains("https://example.test/issues/5"));
        assert!(links.contains("design doc"));
        let duplicate = create_task(&root, NewTask {
            id: 1,
            title: "Different slug".into(),
            description: String::new(),
            criteria: Vec::new(),
            issue: None,
            issue_markdown: None,
            links: Vec::new(),
        });
        assert!(duplicate.unwrap_err().to_string().contains("task 1 already exists"));
        let archived = archive_task(&root, "1").unwrap();
        assert!(archived.join("status.json").is_file());
        assert!(list_tasks(&root, false).unwrap().is_empty());
        assert_eq!(list_tasks(&root, true).unwrap().len(), 1);
        let next = create_task(&root, NewTask {
            id: 0,
            title: "Next task".into(),
            description: String::new(),
            criteria: Vec::new(),
            issue: None,
            issue_markdown: None,
            links: Vec::new(),
        }).unwrap();
        assert_eq!(next.status.id, 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matches_the_shared_go_rust_conformance_fixture() {
        #[derive(Deserialize)]
        struct Fixture {
            plan: String,
            fallback_title: String,
            subtasks: Vec<ExpectedSubtask>,
            review: String,
            want_criteria: usize,
            acceptance: String,
            criteria_count: usize,
        }
        #[derive(Deserialize)]
        struct ExpectedSubtask {
            number: u32,
            title: String,
            depends_on: Vec<u32>,
            verify: Vec<String>,
        }

        let fixture: Fixture = serde_json::from_str(include_str!("../assets/testdata/workflow-conformance.json")).unwrap();
        let parsed = parse_plan(&fixture.plan, &fixture.fallback_title);
        assert_eq!(parsed.len(), fixture.subtasks.len());
        for (got, want) in parsed.iter().zip(&fixture.subtasks) {
            assert_eq!(got.number, want.number);
            assert_eq!(got.title, want.title);
            assert_eq!(got.depends_on, want.depends_on);
            assert_eq!(got.verify, want.verify);
        }
        let verdict = parse_review(&fixture.review, fixture.want_criteria);
        assert!(verdict.found && verdict.approve);
        assert_eq!(verdict.criteria.len(), fixture.want_criteria);
        assert_eq!(count_criteria(&fixture.acceptance), fixture.criteria_count);
    }

    #[test]
    fn scheduler_respects_dependencies_capacity_and_failed_prerequisites() {
        let mut first = Subtask::pending(1, "first", "");
        let mut second = Subtask::pending(2, "second", "");
        second.depends_on = vec![1];
        let mut third = Subtask::pending(3, "third", "");
        let mut subtasks = vec![first.clone(), second.clone(), third.clone()];

        let round = schedule_subtasks(&mut subtasks, &[], 1);
        assert_eq!(round.ready, [1]);
        assert!(!round.stalled);

        subtasks[0].state = "pr".into();
        let round = schedule_subtasks(&mut subtasks, &[], 4);
        assert_eq!(round.ready, [2, 3]);

        first.state = "failed".into();
        second.state = "pending".into();
        third.state = "pr".into();
        let mut blocked = vec![first, second, third];
        let round = schedule_subtasks(&mut blocked, &[], 4);
        assert_eq!(round.dependency_failures, [(2, 1)]);
        assert_eq!(blocked[1].state, "failed");
        assert!(round.stalled);
    }

    #[test]
    fn loads_legacy_agent_arrays_recovers_ids_and_saves_private_registry() {
        let path = std::env::temp_dir().join(format!("shepherd-agents-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let legacy = r#"[{"id":"coder-4","seq":4,"role":"coder","vendor":"codex","root":"/repo","mcp":true,"state":"done","token":"secret","started":"2026-11-03T10:00:00Z","updated":"2026-11-03T10:01:00Z"}]"#;
        std::fs::write(&path, legacy).unwrap();
        let mut registry = load_agent_registry(&path).unwrap();
        assert_eq!(registry.seq, 4);
        assert_eq!(registry.next.get("coder"), Some(&4));
        assert!(registry.agents[0].terminal());
        assert_eq!(registry.agents[0].public().token, "");

        let created = registry.create(Agent { role: "coder".into(), vendor: "claude".into(), root: "/repo".into(), ..Agent::default() }, "fresh-secret".into(), "2026-11-03T10:02:00Z");
        assert_eq!(created.id, "coder-5");
        assert_eq!(created.seq, 5);
        assert_eq!(created.state, "starting");
        assert_eq!(registry.live().len(), 1);
        assert!(registry.by_token("fresh-secret").is_some());
        assert!(registry.by_token("").is_none());
        assert!(registry.remove("coder-5"));
        assert_eq!(registry.create(Agent { role: "coder".into(), ..Agent::default() }, "another-secret".into(), "now").id, "coder-6");
        save_agent_registry(&path, &registry).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved.is_object());
        assert_eq!(saved["seq"], 6);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn generated_agent_tokens_are_random_hex_and_redacted_from_public_agents() {
        let mut registry = AgentRegistryFile::default();
        let agent = registry.create_secure(Agent { role: "reviewer".into(), ..Agent::default() }, "now").unwrap();
        assert_eq!(agent.token.len(), 48);
        assert!(agent.token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(agent.public().token, "");
    }

    #[test]
    fn submit_result_records_summary_once_as_a_terminal_done_state() {
        let mut registry = AgentRegistryFile::default();
        let agent = registry.create(Agent { role: "coder".into(), root: "/repo".into(), ..Agent::default() }, "token".into(), "start");
        assert!(registry.submit_result(&agent.id, "  implemented and tested  ", "finish").is_ok());
        let result = registry.result(&agent.id).unwrap();
        assert_eq!(result.state, "done");
        assert!(result.terminal());
        assert!(result.has_result);
        assert_eq!(result.result, "implemented and tested");
        assert_eq!(result.updated, "finish");
        assert_eq!(registry.submit_result("missing-1", "result", "finish"), Err("unknown agent \"missing-1\"".into()));
        assert_eq!(registry.submit_result(&agent.id, "  ", "finish"), Err("result summary is empty".into()));
    }

    #[test]
    fn json_schemas_for_agents_tasks_and_roles_are_stable_and_never_leak_tokens() {
        let mut registry = AgentRegistryFile::default();
        registry.create(
            Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                model: "gpt-5".into(),
                task_id: "1".into(),
                task_text: "Implement JSON output".into(),
                root: "/repo".into(),
                mcp: true,
                pane: "w1:p1".into(),
                ..Agent::default()
            },
            "super-secret-agent-token-999".into(),
            "2026-11-03T10:00:00Z",
        );
        let raw_agents = agents_json(&registry).unwrap();
        assert!(
            !raw_agents.contains("super-secret-agent-token-999"),
            "agents_json must never leak agent tokens"
        );
        assert!(
            !raw_agents.contains("\"token\""),
            "agents_json must omit the token field completely"
        );
        let parsed_agents: serde_json::Value = serde_json::from_str(&raw_agents).unwrap();
        assert!(parsed_agents.is_array());
        assert_eq!(parsed_agents[0]["id"], "coder-1");
        assert_eq!(parsed_agents[0]["role"], "coder");
        assert_eq!(parsed_agents[0]["vendor"], "codex");
        assert_eq!(parsed_agents[0]["model"], "gpt-5");
        assert_eq!(parsed_agents[0]["state"], "starting");
        assert!(parsed_agents[0].get("token").is_none());

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "shepherd-json-schema-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let task = create_task(
            &root,
            NewTask {
                id: 3,
                title: "JSON Schema".into(),
                description: "Verify JSON schemas".into(),
                criteria: vec!["No tokens".into()],
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();
        let tasks = list_tasks(&root, false).unwrap();
        let raw_list = task_list_json(&tasks).unwrap();
        let parsed_list: serde_json::Value = serde_json::from_str(&raw_list).unwrap();
        assert!(parsed_list.is_array());
        assert_eq!(parsed_list[0]["id"], 3);
        assert_eq!(parsed_list[0]["slug"], "json-schema");
        assert_eq!(parsed_list[0]["title"], "JSON Schema");
        assert_eq!(parsed_list[0]["state"], "new");
        assert_eq!(
            parsed_list[0]["directory"],
            task.directory.display().to_string()
        );

        let raw_show = task_show_json(&task, None).unwrap();
        let parsed_show: serde_json::Value = serde_json::from_str(&raw_show).unwrap();
        assert_eq!(parsed_show["id"], 3);
        assert_eq!(
            parsed_show["directory"],
            task.directory.display().to_string()
        );
        assert!(parsed_show["sections"]["issue"]
            .as_str()
            .unwrap()
            .contains("Verify JSON schemas"));
        assert!(parsed_show["sections"]["spec"]
            .as_str()
            .unwrap()
            .contains("# Spec"));
        assert!(parsed_show["context"]
            .as_str()
            .unwrap()
            .contains("# Task 3-json-schema"));

        let raw_section = task_show_json(&task, Some("spec")).unwrap();
        let parsed_section: serde_json::Value = serde_json::from_str(&raw_section).unwrap();
        assert_eq!(parsed_section["id"], 3);
        assert_eq!(parsed_section["slug"], "json-schema");
        assert_eq!(parsed_section["section"], "spec");
        assert!(parsed_section["path"]
            .as_str()
            .unwrap()
            .ends_with("spec.md"));
        assert!(parsed_section["content"]
            .as_str()
            .unwrap()
            .contains("# Spec"));

        let cfg = config::Config::default();
        let active = vec!["claude".into(), "codex".into()];
        let raw_roles = roles_list_json(&cfg, &active).unwrap();
        let parsed_roles: serde_json::Value = serde_json::from_str(&raw_roles).unwrap();
        assert!(parsed_roles.is_array());
        let brain_entry = parsed_roles
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["role"] == "brain")
            .unwrap();
        assert_eq!(brain_entry["configured_vendor"], "claude");
        assert_eq!(brain_entry["vendor"], "claude");
        assert_eq!(brain_entry["model"], "opus");
        assert_eq!(brain_entry["available"], true);
        assert_eq!(brain_entry["can_spawn"], true);

        let docs_entry = parsed_roles
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["role"] == "docs")
            .unwrap();
        assert_eq!(docs_entry["configured_vendor"], "gemini");
        assert_eq!(docs_entry["available"], true);
        assert_eq!(docs_entry["fallback"], true);

        let parsed_unavail: serde_json::Value =
            serde_json::from_str(&roles_list_json(&cfg, &[]).unwrap()).unwrap();
        assert_eq!(parsed_unavail[0]["available"], false);
        assert!(parsed_unavail[0]["error"].as_str().is_some());

        std::fs::remove_dir_all(root).unwrap();
    }
}
