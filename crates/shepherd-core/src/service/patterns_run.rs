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
    /// F12: persona prompt of the pattern definition (served by `get_task_context`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// F12: this agent starts the run (receives the objective as its kick-off order).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start: bool,
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
    #[serde(alias = "completed")]
    Done,
    Degraded,
    /// F12: agents are idle/blocked but the run did not finish healthily (blocked agent or no
    /// message ever published). A new message returns it to `running`.
    Stalled,
}

impl std::fmt::Display for PatternRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
            Self::Done => write!(f, "done"),
            Self::Degraded => write!(f, "degraded"),
            Self::Stalled => write!(f, "stalled"),
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
    /// `done` reached by quiescence (agents idle, nothing published for T seconds); a new message revives it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quiesced: bool,
    /// F12: why the run is `stalled` (blocked agent / no activity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_reason: Option<String>,
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

/// F8g: a publish on a topic of a quiesced `done` (or F12 `stalled`) run reopens it (event-driven, so a fast agent
/// that is idle and acked again before the next reconcile cannot miss it). The quiet timer is
/// already cleared at `done`, so the run returns to `done` only once it calms down again.
/// Free function: MCP agent publishes reach the bus without an `OrchestrationService`.
pub(crate) fn reopen_quiesced_runs(
    state_dir: &Path,
    registry: &crate::AgentRegistryFile,
    topic: &str,
    now: &str,
) -> Result<(), String> {
    let mut paths = vec![pattern_runs_path(state_dir, state_dir)];
    for ag in &registry.agents {
        if !ag.root.is_empty() {
            paths.push(pattern_runs_path(Path::new(&ag.root), state_dir));
        }
    }
    paths.sort();
    paths.dedup();
    for path in paths {
        let Ok(mut runs) = load_pattern_runs(&path) else {
            continue;
        };
        let mut dirty = false;
        for run in &mut runs {
            let revivable = (run.state == PatternRunState::Done && run.quiesced)
                || run.state == PatternRunState::Stalled;
            if revivable && run.topics.iter().any(|t| t == topic) {
                run.state = PatternRunState::Running;
                run.quiesced = false;
                run.stalled_reason = None;
                run.finished_at = None;
                run.updated_at = now.to_string();
                dirty = true;
            }
        }
        if dirty {
            save_pattern_runs(&path, &runs)?;
        }
    }
    Ok(())
}

/// F12: `get_task_context` of an agent whose `task_id` is a pattern run id: objective, topology
/// and the agent's persona instead of a "no task" error. `None` when `run_id` is not a run.
pub(crate) fn pattern_run_context(
    state_dir: &Path,
    root: &Path,
    run_id: &str,
    agent_id: &str,
) -> Option<String> {
    for path in [
        pattern_runs_path(root, state_dir),
        pattern_runs_path(state_dir, state_dir),
    ] {
        if let Some(run) = load_pattern_runs(&path)
            .ok()
            .and_then(|runs| runs.into_iter().find(|r| r.id == run_id))
        {
            return Some(super::pattern_prompt::run_context(&run, agent_id));
        }
    }
    None
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
    serde_json::from_slice(&data)
        .map_err(|e| format!("invalid pattern runs json in {}: {e}", path.display()))
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

use super::{load_config, OrchestrationService};

impl OrchestrationService {
    pub fn start_pattern_run(
        &self,
        root: &Path,
        pattern_name: Option<&str>,
        pattern_obj: Option<crate::patterns::Pattern>,
        task: Option<&str>,
        worktree: Option<&str>,
        confirm_over_limit: bool,
    ) -> Result<PatternRun, String> {
        self.start_pattern_run_in(
            root,
            pattern_name,
            pattern_obj,
            task,
            worktree,
            confirm_over_limit,
            None,
        )
    }

    /// Like `start_pattern_run`, with every agent placed in `workspace` (the one the run was launched from).
    #[allow(clippy::too_many_arguments)] // mirrors start_pattern_run plus the workspace
    pub fn start_pattern_run_in(
        &self,
        root: &Path,
        pattern_name: Option<&str>,
        pattern_obj: Option<crate::patterns::Pattern>,
        task: Option<&str>,
        worktree: Option<&str>,
        confirm_over_limit: bool,
        workspace: Option<&str>,
    ) -> Result<PatternRun, String> {
        let pattern = if let Some(pat) = pattern_obj {
            pat
        } else if let Some(name) = pattern_name {
            if let Some(builtin) = crate::patterns::get_builtin_pattern(name) {
                builtin
            } else {
                match crate::patterns::load_pattern(name, root, None) {
                    Ok((pat, _, _)) => pat,
                    Err(e) => return Err(format!("cannot load pattern {name:?}: {e}")),
                }
            }
        } else {
            return Err("missing 'pattern' or 'pattern_obj' for pattern run".to_owned());
        };

        let config = load_config(root).unwrap_or_default();
        let problems = crate::patterns::validate(&pattern, &config);
        let errors: Vec<_> = problems
            .iter()
            .filter(|p| p.severity == crate::patterns::Severity::Error)
            .collect();
        if !errors.is_empty() {
            let msg = errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "validation failed for pattern '{}': {msg}",
                pattern.name
            ));
        }

        // Limits check: hierarchical depth
        if pattern.kind == crate::patterns::PatternKind::Hierarchical {
            for ag in &pattern.agents {
                let lvl = ag.level.unwrap_or(0);
                if lvl > config.limits.max_spawn_depth && !confirm_over_limit {
                    return Err(format!(
                        "confirmation_required: agent '{}' level {} exceeds max_spawn_depth ({}); pass confirm_over_limit to proceed",
                        ag.id, lvl, config.limits.max_spawn_depth
                    ));
                }
            }
        }

        // Limits check: max concurrent agents
        let total_new: usize = pattern.agents.iter().map(|a| a.count.max(1) as usize).sum();
        let registry = self.load_registry()?;
        let live_count = self.active_agent_count(&registry);
        if live_count + total_new > config.limits.max_concurrent_agents && !confirm_over_limit {
            return Err(format!(
                "confirmation_required: max_concurrent_agents ({}) reached ({} active + {} new); pass confirm_over_limit to proceed",
                config.limits.max_concurrent_agents, live_count, total_new
            ));
        }

        // Create pattern topics in bus
        let bus = self.bus()?;
        for topic in &pattern.topics {
            let _ = bus.topic_create(&topic.name, topic.partitions);
        }

        let run_id = format!("run-{}-{}", pattern.name, self.clock.now_ms());
        let now = self.clock.now_rfc3339();
        let task_desc = task.unwrap_or("Execute pattern workflow").to_string();

        let abs_root = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::fs::canonicalize(root)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(root))
        };

        let starters = pattern.starter_ids();
        let mut run_agents: Vec<PatternRunAgent> = Vec::new();
        for agent_def in &pattern.agents {
            let count = agent_def.count.max(1);
            for idx in 0..count {
                let agent_slot_id = if count == 1 {
                    agent_def.id.clone()
                } else {
                    format!("{}-{}", agent_def.id, idx + 1)
                };

                let prompt = super::pattern_prompt::startup_prompt(
                    &pattern,
                    agent_def,
                    &task_desc,
                    starters.contains(&agent_def.id),
                );

                let spawn_req = crate::service::ServiceSpawnRequest {
                    root: abs_root.clone(),
                    role: agent_def.role.clone(),
                    task: task_desc.clone(),
                    pattern_prompt: Some(prompt),
                    task_id: Some(run_id.clone()),
                    vendor: agent_def.vendor.clone(),
                    model: agent_def.model.clone(),
                    parent_id: None,
                    worktree: worktree.map(|w| !w.is_empty()),
                    skills: Default::default(),
                    mcp_url: self.mcp_url(),
                    confirm_over_limit,
                    deliver_prompt: true,
                    workspace: workspace.map(str::to_owned),
                };

                let spawned = match self.spawn(spawn_req) {
                    Ok(ag) => ag,
                    Err(e) => {
                        // Nothing is left half-created: the agents already started for this run are closed.
                        for started in &run_agents {
                            let _ = self.kill(&started.id);
                        }
                        let failed_run = PatternRun {
                            id: run_id.clone(),
                            pattern: pattern.name.clone(),
                            kind: pattern.kind.to_string(),
                            task: task_desc.clone(),
                            worktree: worktree.map(|w| w.to_string()),
                            topics: pattern.topics.iter().map(|t| t.name.clone()).collect(),
                            agents: run_agents,
                            state: PatternRunState::Failed,
                            created_at: now.clone(),
                            updated_at: now.clone(),
                            finished_at: Some(now),
                            error: Some(e.clone()),
                            quiesced: false,
                            stalled_reason: None,
                        };
                        let runs_file = pattern_runs_path(root, &self.service_config.state_dir);
                        let mut existing = load_pattern_runs(&runs_file).unwrap_or_default();
                        existing.push(failed_run);
                        let _ = save_pattern_runs(&runs_file, &existing);
                        return Err(format!("failed to spawn agent {agent_slot_id}: {e}"));
                    }
                };

                for topic_name in &agent_def.consumes {
                    let group = agent_def.group.as_deref().unwrap_or(&agent_def.role);
                    let _ = bus.subscribe(&spawned.id, topic_name, Some(group));
                }

                run_agents.push(PatternRunAgent {
                    id: spawned.id,
                    role: agent_def.role.clone(),
                    vendor: Some(spawned.vendor.clone()),
                    model: Some(spawned.model.clone()),
                    level: agent_def.level,
                    state: spawned.state.clone(),
                    prompt: agent_def.prompt.clone(),
                    start: starters.contains(&agent_def.id),
                    publishes: agent_def.publishes.clone(),
                    consumes: agent_def.consumes.clone(),
                });
            }
        }

        let run = PatternRun {
            id: run_id,
            pattern: pattern.name.clone(),
            kind: pattern.kind.to_string(),
            task: task_desc,
            worktree: worktree.map(|w| w.to_string()),
            topics: pattern.topics.iter().map(|t| t.name.clone()).collect(),
            agents: run_agents,
            state: PatternRunState::Running,
            created_at: now.clone(),
            updated_at: now,
            finished_at: None,
            error: None,
            quiesced: false,
            stalled_reason: None,
        };

        let runs_file = pattern_runs_path(root, &self.service_config.state_dir);
        let mut existing = load_pattern_runs(&runs_file).unwrap_or_default();
        existing.push(run.clone());
        save_pattern_runs(&runs_file, &existing)?;
        Ok(run)
    }

    /// F12: live agents that count for `max_concurrent_agents`: the `idle`/`done` agents of runs
    /// that are `done`/`stopped`/`stalled` are inactive and do not count.
    pub(crate) fn active_agent_count(&self, registry: &crate::AgentRegistryFile) -> usize {
        let mut paths = vec![pattern_runs_path(
            &self.service_config.state_dir,
            &self.service_config.state_dir,
        )];
        for ag in &registry.agents {
            if !ag.root.is_empty() {
                paths.push(pattern_runs_path(
                    Path::new(&ag.root),
                    &self.service_config.state_dir,
                ));
            }
        }
        paths.sort();
        paths.dedup();
        let mut inactive = std::collections::HashSet::new();
        for path in paths {
            for run in load_pattern_runs(&path).unwrap_or_default() {
                if matches!(
                    run.state,
                    PatternRunState::Done | PatternRunState::Stopped | PatternRunState::Stalled
                ) {
                    inactive.extend(run.agents.into_iter().map(|a| a.id));
                }
            }
        }
        registry
            .live()
            .into_iter()
            .filter(|a| !(inactive.contains(&a.id) && matches!(a.state.as_str(), "idle" | "done")))
            .count()
    }

    /// F8h: total messages ever published on the run's topics. Lag/acks are ignored on purpose:
    /// real agents leave their own summaries and foreign-group messages unacknowledged.
    fn run_published_count(&self, run: &PatternRun) -> usize {
        self.bus()
            .and_then(|b| b.list_topics().map_err(|e| e.to_string()))
            .map(|ts| {
                ts.iter()
                    .filter(|t| run.topics.contains(&t.name))
                    .map(|t| t.messages_count)
                    .sum()
            })
            .unwrap_or(0)
    }

    pub fn reconcile_pattern_runs(
        &self,
        registry: &crate::AgentRegistryFile,
        root: Option<&Path>,
    ) -> Result<(), String> {
        let mut paths = vec![pattern_runs_path(
            &self.service_config.state_dir,
            &self.service_config.state_dir,
        )];
        if let Some(r) = root {
            paths.push(pattern_runs_path(r, &self.service_config.state_dir));
        }
        for ag in &registry.agents {
            if !ag.root.is_empty() {
                paths.push(pattern_runs_path(
                    Path::new(&ag.root),
                    &self.service_config.state_dir,
                ));
            }
        }
        paths.sort();
        paths.dedup();

        let now = self.clock.now_rfc3339();

        for path in paths {
            if !path.exists() {
                continue;
            }
            let mut runs = match load_pattern_runs(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let mut dirty = false;

            for run in &mut runs {
                let revivable = (run.state == PatternRunState::Done && run.quiesced)
                    || run.state == PatternRunState::Stalled;
                if run.state != PatternRunState::Running
                    && run.state != PatternRunState::Paused
                    && !revivable
                {
                    continue;
                }

                for ag in &mut run.agents {
                    if let Some(live_agent) = registry.agents.iter().find(|a| a.id == ag.id) {
                        ag.state = live_agent.state.clone();
                    } else {
                        ag.state = "exited".to_string();
                    }
                }

                let live_count = run
                    .agents
                    .iter()
                    .filter(|a| {
                        !matches!(a.state.as_str(), "done" | "exited" | "killed" | "failed")
                    })
                    .count();
                let busy = run
                    .agents
                    .iter()
                    .any(|a| matches!(a.state.as_str(), "working" | "starting"));
                // F12: blocked agents (or agents that reported an error) do not keep the run
                // alive forever; once it calms down the run is `stalled`, not `done`.
                let blocked: Vec<String> = run
                    .agents
                    .iter()
                    .filter(|a| {
                        a.state == "blocked"
                            || registry
                                .agents
                                .iter()
                                .any(|r| r.id == a.id && !r.error.is_empty() && !r.terminal())
                    })
                    .map(|a| a.id.clone())
                    .collect();

                if revivable {
                    if busy {
                        run.state = PatternRunState::Running;
                        run.quiesced = false;
                        run.stalled_reason = None;
                        run.finished_at = None;
                        run.updated_at = now.clone();
                        dirty = true;
                    }
                    continue;
                }

                if live_count == 0 {
                    self.run_quiet_since.lock().unwrap().remove(&run.id);
                    let all_done =
                        !run.agents.is_empty() && run.agents.iter().all(|a| a.state == "done");
                    let all_failed = !run.agents.is_empty()
                        && run
                            .agents
                            .iter()
                            .all(|a| matches!(a.state.as_str(), "exited" | "killed" | "failed"));

                    if all_done {
                        run.state = PatternRunState::Done;
                    } else if all_failed {
                        run.state = PatternRunState::Failed;
                    } else {
                        run.state = PatternRunState::Degraded;
                    }

                    if run.finished_at.is_none() {
                        run.finished_at = Some(now.clone());
                    }
                    run.updated_at = now.clone();
                    dirty = true;
                } else if run.state == PatternRunState::Running {
                    // Quiescence (F8h): every agent idle/done and nothing published on the run's
                    // topics for `run_quiescence_secs`. Lag/acks do not count.
                    let period_ms = self.service_config.run_quiescence_secs.saturating_mul(1000);
                    if period_ms == 0 || busy {
                        self.run_quiet_since.lock().unwrap().remove(&run.id);
                    } else {
                        let now_ms = self.clock.now_ms();
                        let published = self.run_published_count(run);
                        let mut quiet = self.run_quiet_since.lock().unwrap();
                        let entry = quiet.entry(run.id.clone()).or_insert((now_ms, published));
                        if entry.1 != published {
                            *entry = (now_ms, published);
                        }
                        if now_ms.saturating_sub(entry.0) >= period_ms {
                            quiet.remove(&run.id);
                            drop(quiet);
                            if !blocked.is_empty() || published == 0 {
                                run.state = PatternRunState::Stalled;
                                run.stalled_reason = Some(if blocked.is_empty() {
                                    "sin actividad: ningún mensaje publicado en los tópicos del run"
                                        .to_owned()
                                } else {
                                    format!("agente bloqueado: {}", blocked.join(", "))
                                });
                            } else {
                                run.state = PatternRunState::Done;
                                run.quiesced = true;
                                run.finished_at = Some(now.clone());
                            }
                            run.updated_at = now.clone();
                            dirty = true;
                        }
                    }
                } else {
                    self.run_quiet_since.lock().unwrap().remove(&run.id);
                }
            }

            if dirty {
                save_pattern_runs(&path, &runs)?;
            }
        }

        Ok(())
    }

    pub fn list_pattern_runs(&self, root: &Path) -> Result<Vec<PatternRunSummary>, String> {
        let registry = self.load_registry().unwrap_or_default();
        let _ = self.reconcile_pattern_runs(&registry, Some(root));
        let runs_file = pattern_runs_path(root, &self.service_config.state_dir);
        let runs = load_pattern_runs(&runs_file)?;
        let live_ids: std::collections::HashSet<String> =
            registry.live().into_iter().map(|a| a.id.clone()).collect();

        let summaries = runs
            .into_iter()
            .map(|r| {
                let live_count = r.agents.iter().filter(|a| live_ids.contains(&a.id)).count();
                r.summary(live_count)
            })
            .collect();
        Ok(summaries)
    }

    pub fn get_pattern_run(&self, root: &Path, id: &str) -> Result<PatternRun, String> {
        let registry = self.load_registry().unwrap_or_default();
        let _ = self.reconcile_pattern_runs(&registry, Some(root));
        let runs_file = pattern_runs_path(root, &self.service_config.state_dir);
        let runs = load_pattern_runs(&runs_file)?;
        let mut run = runs
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("pattern run '{id}' not found"))?;

        for ag in &mut run.agents {
            if let Some(live_agent) = registry.agents.iter().find(|a| a.id == ag.id) {
                ag.state = live_agent.state.clone();
            }
        }
        Ok(run)
    }

    pub fn pause_pattern_run(&self, root: &Path, id: &str) -> Result<PatternRun, String> {
        let runs_file = pattern_runs_path(root, &self.service_config.state_dir);
        let mut runs = load_pattern_runs(&runs_file)?;
        let run = runs
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("pattern run '{id}' not found"))?;

        if run.state != PatternRunState::Stopped && run.state != PatternRunState::Failed {
            run.state = PatternRunState::Paused;
            run.updated_at = self.clock.now_rfc3339();
            let bus = self.bus()?;
            for topic in &run.topics {
                let _ = bus.pause_topic(topic);
            }
        }
        let result = run.clone();
        save_pattern_runs(&runs_file, &runs)?;
        Ok(result)
    }

    pub fn resume_pattern_run(&self, root: &Path, id: &str) -> Result<PatternRun, String> {
        let runs_file = pattern_runs_path(root, &self.service_config.state_dir);
        let mut runs = load_pattern_runs(&runs_file)?;
        let run = runs
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("pattern run '{id}' not found"))?;

        if run.state == PatternRunState::Paused {
            run.state = PatternRunState::Running;
            run.updated_at = self.clock.now_rfc3339();
            let bus = self.bus()?;
            for topic in &run.topics {
                let _ = bus.resume_topic(topic);
            }
        }
        let result = run.clone();
        save_pattern_runs(&runs_file, &runs)?;
        Ok(result)
    }

    pub fn stop_pattern_run(
        &self,
        root: &Path,
        id: &str,
        confirm: bool,
    ) -> Result<PatternRun, String> {
        if !confirm {
            return Err(format!(
                "confirmation_required: stopping pattern run '{id}' kills all its agents; pass confirm: true to proceed"
            ));
        }
        let runs_file = pattern_runs_path(root, &self.service_config.state_dir);
        let mut runs = load_pattern_runs(&runs_file)?;
        let run = runs
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("pattern run '{id}' not found"))?;

        run.state = PatternRunState::Stopped;
        let now = self.clock.now_rfc3339();
        run.finished_at = Some(now.clone());
        run.updated_at = now;

        for ag in &mut run.agents {
            let _ = self.kill(&ag.id);
            ag.state = "stopped".to_string();
        }

        if let Ok(bus) = self.bus() {
            for topic in &run.topics {
                let _ = bus.resume_topic(topic);
            }
        }

        let result = run.clone();
        save_pattern_runs(&runs_file, &runs)?;
        Ok(result)
    }
}
