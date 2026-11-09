//! Rust domain core for Shepherd.
//!
//! This crate is intentionally independent of terminal, filesystem, and AI vendor
//! integrations. Those are adapters around the same workflow rules.

pub mod bus;
pub mod cli;
pub mod config;
pub mod git;
pub mod mcp;
pub mod patterns;
pub mod plan;
pub mod service;
pub mod shepherd;
pub mod skills;
pub mod snapshot;
pub mod spawn;
pub mod tasks;
pub mod terminals;
pub mod workflow;

pub use plan::*;
pub use tasks::*;

pub use git::{
    collect_git_status, collect_git_status_with_base, GitPrSummary, GitRepoStatus, GitWorktreeInfo,
    ParsedGitStatus, ParsedPr, ParsedWorktree,
};

pub use cli::{
    run_cli, run_cli_in_root, run_cli_in_root_with_client, run_cli_in_root_with_output,
    run_cli_with_terminals,
};

#[cfg(test)]
pub(crate) static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use patterns::{
    Pattern, PatternAgent, PatternDetail, PatternError, PatternKind, PatternSource, PatternSummary,
    Problem, Severity, Topic,
};

pub use service::{RunSubtaskSummary, RunSummary};

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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

impl Subtask {
    pub(crate) fn pending(number: u32, title: impl Into<String>, body: impl Into<String>) -> Self {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IssueRef {
    pub number: u32,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
}

/// On-disk task state, kept compatible with Go's `.shepherd/tasks/*/status.json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillAdjust {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

/// Shepherd's identity and workflow metadata for an agent. Shepherd remains the owner of its terminal process.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Agent {
    pub id: String,
    #[serde(default)]
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
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
    #[serde(default)]
    pub delivery_cursor: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub started: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated: String,
    /// Snapshots stored for this agent (filled in when listing; omitted when 0).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub snapshots: u32,
    /// Id of the agent's newest snapshot (filled in when listing; omitted when none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_snapshot: String,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl Agent {
    pub fn terminal(&self) -> bool {
        matches!(self.state.as_str(), "done" | "exited" | "killed" | "failed")
    }
    pub fn gone(&self) -> bool {
        matches!(self.state.as_str(), "exited" | "killed" | "failed")
    }
    pub fn public(&self) -> Self {
        let mut agent = self.clone();
        agent.token.clear();
        agent
    }
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
        if summary.trim().is_empty() {
            return Err("result summary is empty".into());
        }
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
        let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == id) else {
            return Err(format!("unknown agent {id:?}"));
        };
        agent.result.clear();
        agent.has_result = false;
        agent.error.clear();
        agent.state = "working".into();
        agent.updated = now.to_owned();
        Ok(())
    }

    pub fn live(&self) -> Vec<&Agent> {
        self.agents
            .iter()
            .filter(|agent| !agent.terminal())
            .collect()
    }

    pub fn by_token(&self, token: &str) -> Option<&Agent> {
        if token.is_empty() {
            return None;
        }
        self.agents.iter().find(|agent| agent.token == token)
    }
}

pub fn new_agent_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

pub fn load_agent_registry(
    path: impl AsRef<Path>,
) -> Result<AgentRegistryFile, Box<dyn std::error::Error + Send + Sync>> {
    let path = path.as_ref();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AgentRegistryFile::default())
        }
        Err(error) => return Err(error.into()),
    };
    let mut registry = if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'[') {
        AgentRegistryFile {
            agents: serde_json::from_slice(&bytes)?,
            ..AgentRegistryFile::default()
        }
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
            if let Some(number) = agent
                .id
                .strip_prefix(&format!("{}-", agent.role))
                .and_then(|n| n.parse::<u64>().ok())
            {
                self.next
                    .entry(agent.role.clone())
                    .and_modify(|n| *n = (*n).max(number))
                    .or_insert(number);
            }
        }
    }
}

pub fn save_agent_registry(
    path: impl AsRef<Path>,
    registry: &AgentRegistryFile,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
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

fn is_zero(value: &u32) -> bool {
    *value == 0
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreatedTask {
    pub status: TaskStatus,
    pub directory: std::path::PathBuf,
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
    use crate::plan::field_value;

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
    fn field_value_tolerates_markdown_bold_and_list_markers() {
        assert_eq!(field_value("- **Verify:** `cmd`", "verify"), Some("`cmd`"));
        assert_eq!(field_value("**Verify**: cmd", "verify"), Some("cmd"));
        assert_eq!(
            field_value("* **Depends on:** 1, 2", "depends on"),
            Some("1, 2")
        );
        assert_eq!(field_value("Verify: cmd", "verify"), Some("cmd"));
        assert_eq!(field_value("- Verify: `cmd`", "verify"), Some("`cmd`"));
        assert_eq!(field_value("- Verify: cmd", "verify"), Some("cmd"));
        assert_eq!(
            field_value("* Depends on: 1, 2", "depends on"),
            Some("1, 2")
        );
        assert_eq!(field_value("Depends on: 1, 2", "depends on"), Some("1, 2"));
        assert_eq!(
            field_value("* **Depends on**: 1, 2", "depends on"),
            Some("1, 2")
        );
        assert_eq!(
            field_value("- **Depends on**: 1, 2", "depends on"),
            Some("1, 2")
        );
        assert_eq!(
            field_value("**Depends on:** 1, 2", "depends on"),
            Some("1, 2")
        );
        assert_eq!(
            field_value("**Depends on**: 1, 2", "depends on"),
            Some("1, 2")
        );
        assert_eq!(
            field_value("- **Depends on:** none", "depends on"),
            Some("none")
        );
        assert_eq!(
            field_value("- **Depends on:** **none**", "depends on"),
            Some("none")
        );
        assert_eq!(
            field_value(
                "- **Verify:** `printf 'hello from shepherd' | cmp - hello.txt && echo OK`",
                "verify"
            ),
            Some("`printf 'hello from shepherd' | cmp - hello.txt && echo OK`")
        );
        assert_eq!(field_value("Verify: ls *", "verify"), Some("ls *"));
        assert_eq!(
            field_value("Verify: echo 'a: b'", "verify"),
            Some("echo 'a: b'")
        );
        assert_eq!(
            field_value("- **Verify:** **`cmd`**", "verify"),
            Some("`cmd`")
        );
        assert_eq!(field_value("__Verify:__ `cmd`", "verify"), Some("`cmd`"));
        assert_eq!(field_value("__Verify__: cmd", "verify"), Some("cmd"));
        assert_eq!(
            field_value("**VERDICT:** APPROVE", "verdict"),
            Some("APPROVE")
        );
        assert_eq!(
            field_value("**VERDICT**: APPROVE", "verdict"),
            Some("APPROVE")
        );
        assert_eq!(
            field_value("VERDICT: **APPROVE**", "verdict"),
            Some("APPROVE")
        );
        assert_eq!(
            field_value("**VERDICT:** **APPROVE**", "verdict"),
            Some("APPROVE")
        );
    }

    #[test]
    fn parse_plan_handles_markdown_bold_verify_and_dependencies() {
        let plan = r#"
### 1. First
- **Verify:** `printf 'hello from shepherd' | cmp - hello.txt && echo OK`

### 2. Second
**Verify**: cmd
* **Depends on:** 1

### 3. Third
Verify: plain_cmd
Depends on: 1, 2
"#;
        let parsed = parse_plan(plan, "fallback");
        assert_eq!(parsed.len(), 3);
        assert_eq!(
            parsed[0].verify,
            ["printf 'hello from shepherd' | cmp - hello.txt && echo OK"]
        );
        assert_eq!(parsed[1].verify, ["cmd"]);
        assert_eq!(parsed[1].depends_on, [1]);
        assert_eq!(parsed[2].verify, ["plain_cmd"]);
        assert_eq!(parsed[2].depends_on, [1, 2]);
    }

    #[test]
    fn detects_dependency_cycle_and_duplicate_ids() {
        let mut a = Subtask::pending(1, "a", "");
        let mut b = Subtask::pending(2, "b", "");
        a.depends_on = vec![2];
        b.depends_on = vec![1];
        assert!(matches!(
            validate_plan(&[a.clone(), b.clone()]),
            Err(PlanError::DependencyCycle(_))
        ));
        assert_eq!(
            validate_plan(&[a.clone(), a]),
            Err(PlanError::DuplicateSubtask(1))
        );
    }

    #[test]
    fn requires_every_acceptance_criterion_for_approval() {
        let review = "VERDICT: APPROVE\n\nCRITERIA\n- [x] login works\n- [ ] refresh works\n\nSUMMARY\nLooks good";
        let verdict = parse_review(review, 2);
        assert!(!verdict.approve);
        assert_eq!(
            verdict.downgraded.as_deref(),
            Some("approved with unmet criteria")
        );
    }

    #[test]
    fn ignores_acceptance_template_placeholder() {
        assert_eq!(
            count_criteria("- [ ] (add checkable criteria; reviewer verifies)"),
            0
        );
        assert_eq!(count_criteria("- [ ] first\n- [x] second"), 2);
    }

    #[test]
    fn aggregates_subtask_states_with_work_in_progress_priority() {
        let mut first = Subtask::pending(1, "first", "");
        let mut second = Subtask::pending(2, "second", "");
        first.state = "pr".into();
        second.state = "reviewing".into();
        assert_eq!(
            aggregate_task_state(&[first.clone(), second.clone()], "new"),
            "reviewing"
        );
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
        assert_eq!(
            subs[1].note.as_deref(),
            Some("resumed after an interruption")
        );
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
        }))
        .unwrap();
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

        let path =
            std::env::temp_dir().join(format!("shepherd-status-{}.json", std::process::id()));
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
        let root =
            std::env::temp_dir().join(format!("shepherd-create-task-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let task = create_task(
            &root,
            NewTask {
                id: 0,
                title: "Añadir OAuth: sesión".into(),
                description: "Support login".into(),
                criteria: vec!["login works".into(), "  ".into()],
                issue: Some(IssueRef {
                    number: 5,
                    url: "https://example.test/issues/5".into(),
                    repo: String::new(),
                }),
                issue_markdown: None,
                links: vec!["design doc".into()],
            },
        )
        .unwrap();

        assert_eq!(task.status.id, 1);
        assert_eq!(task.directory.file_name().unwrap(), "1-anadir-oauth-sesion");
        assert!(task.directory.join("attachments").is_dir());
        assert!(task.directory.join("status.json").is_file());
        let acceptance = std::fs::read_to_string(task.directory.join("acceptance.md")).unwrap();
        assert_eq!(count_criteria(&acceptance), 1);
        let links = std::fs::read_to_string(task.directory.join("links.md")).unwrap();
        assert!(links.contains("https://example.test/issues/5"));
        assert!(links.contains("design doc"));
        let duplicate = create_task(
            &root,
            NewTask {
                id: 1,
                title: "Different slug".into(),
                description: String::new(),
                criteria: Vec::new(),
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        );
        assert!(duplicate
            .unwrap_err()
            .to_string()
            .contains("task 1 already exists"));
        let archived = archive_task(&root, "1").unwrap();
        assert!(archived.join("status.json").is_file());
        assert!(list_tasks(&root, false).unwrap().is_empty());
        assert_eq!(list_tasks(&root, true).unwrap().len(), 1);
        let next = create_task(
            &root,
            NewTask {
                id: 0,
                title: "Next task".into(),
                description: String::new(),
                criteria: Vec::new(),
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();
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

        let fixture: Fixture =
            serde_json::from_str(include_str!("../assets/testdata/workflow-conformance.json"))
                .unwrap();
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
        let path =
            std::env::temp_dir().join(format!("shepherd-agents-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let legacy = r#"[{"id":"coder-4","seq":4,"role":"coder","vendor":"codex","root":"/repo","mcp":true,"state":"done","token":"secret","started":"2026-11-03T10:00:00Z","updated":"2026-11-03T10:01:00Z"}]"#;
        std::fs::write(&path, legacy).unwrap();
        let mut registry = load_agent_registry(&path).unwrap();
        assert_eq!(registry.seq, 4);
        assert_eq!(registry.next.get("coder"), Some(&4));
        assert!(registry.agents[0].terminal());
        assert_eq!(registry.agents[0].public().token, "");

        let created = registry.create(
            Agent {
                role: "coder".into(),
                vendor: "claude".into(),
                root: "/repo".into(),
                ..Agent::default()
            },
            "fresh-secret".into(),
            "2026-11-03T10:02:00Z",
        );
        assert_eq!(created.id, "coder-5");
        assert_eq!(created.seq, 5);
        assert_eq!(created.state, "starting");
        assert_eq!(registry.live().len(), 1);
        assert!(registry.by_token("fresh-secret").is_some());
        assert!(registry.by_token("").is_none());
        assert!(registry.remove("coder-5"));
        assert_eq!(
            registry
                .create(
                    Agent {
                        role: "coder".into(),
                        ..Agent::default()
                    },
                    "another-secret".into(),
                    "now"
                )
                .id,
            "coder-6"
        );
        save_agent_registry(&path, &registry).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved.is_object());
        assert_eq!(saved["seq"], 6);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn generated_agent_tokens_are_random_hex_and_redacted_from_public_agents() {
        let mut registry = AgentRegistryFile::default();
        let agent = registry
            .create_secure(
                Agent {
                    role: "reviewer".into(),
                    ..Agent::default()
                },
                "now",
            )
            .unwrap();
        assert_eq!(agent.token.len(), 48);
        assert!(agent.token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(agent.public().token, "");
    }

    #[test]
    fn submit_result_records_summary_once_as_a_terminal_done_state() {
        let mut registry = AgentRegistryFile::default();
        let agent = registry.create(
            Agent {
                role: "coder".into(),
                root: "/repo".into(),
                ..Agent::default()
            },
            "token".into(),
            "start",
        );
        assert!(registry
            .submit_result(&agent.id, "  implemented and tested  ", "finish")
            .is_ok());
        let result = registry.result(&agent.id).unwrap();
        assert_eq!(result.state, "done");
        assert!(result.terminal());
        assert!(result.has_result);
        assert_eq!(result.result, "implemented and tested");
        assert_eq!(result.updated, "finish");
        assert_eq!(
            registry.submit_result("missing-1", "result", "finish"),
            Err("unknown agent \"missing-1\"".into())
        );
        assert_eq!(
            registry.submit_result(&agent.id, "  ", "finish"),
            Err("result summary is empty".into())
        );
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
