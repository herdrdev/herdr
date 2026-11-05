//! Pattern run execution, lifecycle state, and persistence.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct PatternRunAgent {
    pub id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publishes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PatternRunState {
    Running,
    Paused,
    Stopped,
    Failed,
    Completed,
}

impl std::fmt::Display for PatternRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
            Self::Completed => write!(f, "completed"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct PatternRun {
    pub id: String,
    pub pattern: String,
    pub kind: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<PatternRunAgent>,
    pub state: PatternRunState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PatternRun {
    pub fn summary(&self, live_agents: usize) -> PatternRunSummary {
        PatternRunSummary {
            id: self.id.clone(),
            pattern: self.pattern.clone(),
            kind: self.kind.clone(),
            state: self.state,
            agents_count: self.agents.len(),
            live_agents,
            topics_count: self.topics.len(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct PatternRunSummary {
    pub id: String,
    pub pattern: String,
    pub kind: String,
    pub state: PatternRunState,
    pub agents_count: usize,
    pub live_agents: usize,
    pub topics_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

/// Determine location for pattern runs storage.
pub fn pattern_runs_path(root: &Path, state_dir: &Path) -> PathBuf {
    let proj = root.join(".shepherd");
    if proj.is_dir() {
        proj.join("pattern_runs.json")
    } else {
        state_dir.join("pattern_runs.json")
    }
}

/// Load list of pattern runs from disk.
pub fn load_pattern_runs(path: &Path) -> Result<Vec<PatternRun>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if data.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&data).map_err(|e| format!("invalid pattern runs json in {}: {e}", path.display()))
}

/// Atomically save pattern runs to disk.
pub fn save_pattern_runs(path: &Path, runs: &[PatternRun]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp_path = PathBuf::from(tmp);

    let bytes = serde_json::to_vec_pretty(runs)
        .map_err(|e| format!("cannot serialize pattern runs: {e}"))?;

    std::fs::write(&tmp_path, bytes)
        .map_err(|e| format!("cannot write {}: {e}", tmp_path.display()))?;

    std::fs::rename(&tmp_path, path)
        .map_err(|e| format!("cannot rename to {}: {e}", path.display()))?;

    Ok(())
}
