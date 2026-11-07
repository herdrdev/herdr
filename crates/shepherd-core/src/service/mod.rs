//! Orchestration service for Shepherd (F2).
//!
//! Owns the durable agent registry (`agents.json`), message bus, periodic
//! reconciliation, startup resume, first-prompt delivery with reaction
//! verification and human-dialog notice, leftover-pane reclamation on name
//! collision, operations (`spawn`, `kill`, `clean`, `focus`, `agents`, `run`,
//! `approve`), and failed-request logging.
//!
//! All time-dependent logic uses the injectable [`Clock`] trait so unit tests
//! can verify deadlines, retries, and quiet-screen windows deterministically
//! without sleeping.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bus::MessageBus;
use crate::config::{self, load_config, Config};
use crate::skills;
use crate::terminals::Terminals;
use crate::{
    git, load_agent_registry, now_rfc3339, save_agent_registry, Agent, AgentRegistryFile,
    CreatedTask, SkillAdjust,
};

/// How long a terminal must stay unresolvable before its agent is declared gone. The server answers
/// `not found` for panes it has just created while it is busy, and reconcile ticks can arrive in bursts.
pub(crate) const MISSING_GRACE_MS: u64 = 20_000;

pub const BLOCKED_HUMAN_NOTICE: &str =
    "esperando al humano: diálogo de confianza o login; abre su pestaña";
pub const NEVER_READY_ERROR: &str = "nunca estuvo listo, probablemente esperaba al humano";

/// Injectable clock for deterministic testing of timeouts, retries, and quiet periods.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
    fn now_rfc3339(&self) -> String;
    fn sleep(&self, duration: Duration);
}

/// Production wall/monotonic clock.
pub struct SystemClock {
    epoch: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn now_rfc3339(&self) -> String {
        now_rfc3339()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

type SleepCallback = Arc<dyn Fn(u64) + Send + Sync>;

/// Deterministic test clock that never sleeps the OS thread.
#[derive(Clone, Default)]
pub struct ManualClock {
    now_ms: Arc<Mutex<u64>>,
    on_sleep: Arc<Mutex<Option<SleepCallback>>>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, duration: Duration) {
        let mut guard = self.now_ms.lock().unwrap();
        *guard = guard.saturating_add(duration.as_millis() as u64);
    }

    pub fn set_on_sleep(&self, hook: impl Fn(u64) + Send + Sync + 'static) {
        *self.on_sleep.lock().unwrap() = Some(Arc::new(hook));
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        *self.now_ms.lock().unwrap()
    }

    fn now_rfc3339(&self) -> String {
        let secs = self.now_ms() / 1000;
        let mins = (secs / 60) % 60;
        let hours = (secs / 3600) % 24;
        let rem_secs = secs % 60;
        format!("2026-04-20T{hours:02}:{mins:02}:{rem_secs:02}Z")
    }

    fn sleep(&self, duration: Duration) {
        let next_ms = {
            let mut guard = self.now_ms.lock().unwrap();
            *guard = guard.saturating_add(duration.as_millis() as u64);
            *guard
        };
        let hook = self.on_sleep.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(next_ms);
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub state_dir: PathBuf,
    pub ready_timeout: Duration,
    pub ready_poll_interval: Duration,
    pub notice_after: Duration,
    pub reaction_timeout: Duration,
    pub reaction_poll_interval: Duration,
    pub max_prompt_resends: usize,
    pub non_mcp_done_quiet_secs: u64,
    /// Seconds a pattern run must stay quiescent before it becomes `done` (0 disables).
    pub run_quiescence_secs: u64,
    pub reconcile_interval: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self::for_state_dir(config::state_dir())
    }
}

impl ServiceConfig {
    pub fn for_state_dir(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            ready_timeout: Duration::from_secs(10 * 60),
            ready_poll_interval: Duration::from_millis(200),
            notice_after: Duration::from_secs(3),
            reaction_timeout: Duration::from_secs(2),
            reaction_poll_interval: Duration::from_millis(100),
            max_prompt_resends: 3,
            non_mcp_done_quiet_secs: 60,
            run_quiescence_secs: 30,
            reconcile_interval: Duration::from_secs(2),
        }
    }

    pub fn registry_path(&self) -> PathBuf {
        self.state_dir.join("agents.json")
    }

    pub fn bus_path(&self) -> PathBuf {
        self.state_dir.join("messages.jsonl")
    }

    pub fn log_path(&self) -> PathBuf {
        self.state_dir.join("service.log")
    }

    pub fn agent_session_dir(&self, agent_id: &str) -> PathBuf {
        self.state_dir
            .join("sessions")
            .join("current")
            .join("agents")
            .join(agent_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedRequestLogEntry {
    pub timestamp: String,
    pub method: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunSubtaskSummary {
    pub n: u32,
    pub title: String,
    pub state: String,
    pub branch: String,
    pub pr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub coder: String,
    pub reviewer: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunSummary {
    pub task_id: u32,
    pub task_title: String,
    pub status: String,
    pub current_stage: String,
    pub has_saved_plan: bool,
    pub pending_gate: Option<serde_json::Value>,
    #[serde(default)]
    pub active_agents: Vec<String>,
    pub subtasks_progress: [usize; 2],
    #[serde(default)]
    pub subtasks: Vec<RunSubtaskSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceSpawnRequest {
    pub root: PathBuf,
    pub role: String,
    pub task: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub worktree: Option<bool>,
    #[serde(default)]
    pub skills: SkillAdjust,
    #[serde(default)]
    pub mcp_url: Option<String>,
    #[serde(default)]
    pub confirm_over_limit: bool,
    #[serde(default = "default_true")]
    pub deliver_prompt: bool,
    /// F12: pattern-mode startup prompt (with `{{agent_id}}` placeholder). When set it replaces
    /// the role's workflow script and the `## Your task` section of the first prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_prompt: Option<String>,
    /// Workspace the agent must join (the one the user launched it from). Wins over the parent's.
    #[serde(
        default,
        alias = "workspace_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace: Option<String>,
}

impl Default for ServiceSpawnRequest {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            role: String::new(),
            task: String::new(),
            task_id: None,
            vendor: None,
            model: None,
            parent_id: None,
            worktree: None,
            skills: SkillAdjust::default(),
            mcp_url: None,
            confirm_over_limit: false,
            deliver_prompt: true,
            pattern_prompt: None,
            workspace: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug)]
pub(crate) struct ScreenQuietTracker {
    pub(crate) last_screen: String,
    pub(crate) unchanged_since_ms: u64,
}

/// Core Shepherd orchestration service.
#[derive(Clone)]
pub struct OrchestrationService {
    pub(crate) service_config: ServiceConfig,
    pub(crate) shepherd_config: Config,
    pub(crate) terminals: Arc<dyn Terminals>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) active_vendors_override: Option<Vec<String>>,
    pub(crate) mcp_url: Arc<Mutex<Option<String>>>,
    pub(crate) blocked_announced: Arc<Mutex<HashSet<String>>>,
    /// Serializes registry writes: the spawn thread, delivery threads and the reconcile loop all save it.
    pub(crate) registry_io: Arc<Mutex<()>>,
    /// Agents removed on purpose, so a merge with the file on disk does not bring them back.
    pub(crate) removed_agents: Arc<Mutex<HashSet<String>>>,
    /// Agent id -> ms of the first reconcile tick in which its terminal could not be found.
    pub(crate) missing_since: Arc<Mutex<HashMap<String, u64>>>,
    /// Highest unread message id an idle MCP agent was already told about.
    pub(crate) nudged: Arc<Mutex<HashMap<String, i64>>>,
    pub(crate) quiet_screens: Arc<Mutex<HashMap<String, ScreenQuietTracker>>>,
    pub(crate) failed_requests: Arc<Mutex<Vec<FailedRequestLogEntry>>>,
    pub(crate) topic_wake_notified: Arc<Mutex<HashMap<(String, String), (u64, u64)>>>,
    /// Run id -> ms since which the run has been continuously quiescent.
    pub(crate) run_quiet_since: Arc<Mutex<HashMap<String, (u64, usize)>>>,
    /// True while a pattern run is still `running`: its quiet period is counted in reconcile ticks, so
    /// the loop ticks every couple of seconds instead of waiting for the next event or backup tick.
    pub(crate) fast_tick: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) auto_snapshots: Arc<snapshots::AutoSnapshots>,
}

impl OrchestrationService {
    pub fn new(
        service_config: ServiceConfig,
        shepherd_config: Config,
        terminals: Arc<dyn Terminals>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let mut svc_cfg = service_config;
        if shepherd_config.limits.notice_after_secs > 0
            && svc_cfg.notice_after == Duration::from_secs(3)
        {
            svc_cfg.notice_after = Duration::from_secs(shepherd_config.limits.notice_after_secs);
        }
        if shepherd_config.limits.non_mcp_done_quiet_secs > 0
            && svc_cfg.non_mcp_done_quiet_secs == 60
        {
            svc_cfg.non_mcp_done_quiet_secs = shepherd_config.limits.non_mcp_done_quiet_secs;
        }
        if svc_cfg.run_quiescence_secs == 30 {
            svc_cfg.run_quiescence_secs = shepherd_config.limits.run_quiescence_secs;
        }
        Self {
            service_config: svc_cfg,
            shepherd_config,
            terminals,
            clock,
            active_vendors_override: None,
            mcp_url: Arc::new(Mutex::new(None)),
            blocked_announced: Arc::new(Mutex::new(HashSet::new())),
            registry_io: Arc::new(Mutex::new(())),
            removed_agents: Arc::new(Mutex::new(HashSet::new())),
            missing_since: Arc::new(Mutex::new(HashMap::new())),
            nudged: Arc::new(Mutex::new(HashMap::new())),
            quiet_screens: Arc::new(Mutex::new(HashMap::new())),
            failed_requests: Arc::new(Mutex::new(Vec::new())),
            topic_wake_notified: Arc::new(Mutex::new(HashMap::new())),
            run_quiet_since: Arc::new(Mutex::new(HashMap::new())),
            fast_tick: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            auto_snapshots: Arc::default(),
        }
    }

    pub fn with_active_vendors(mut self, vendors: Vec<String>) -> Self {
        self.active_vendors_override = Some(vendors);
        self
    }

    pub fn set_mcp_url(&self, url: Option<String>) {
        *self.mcp_url.lock().unwrap() = url;
    }

    pub fn mcp_url(&self) -> Option<String> {
        self.mcp_url.lock().unwrap().clone()
    }

    pub fn service_config(&self) -> &ServiceConfig {
        &self.service_config
    }

    pub fn terminals(&self) -> &Arc<dyn Terminals> {
        &self.terminals
    }

    pub fn bus(&self) -> Result<MessageBus, String> {
        MessageBus::open(self.service_config.bus_path()).map_err(|e| e.to_string())
    }

    /// Empties the message bus: directed messages, topic events, offsets and subscriptions. It would
    /// cut the communication of agents that are still alive, so it needs `force` in that case.
    pub fn clean_bus(&self, force: bool) -> Result<usize, String> {
        let live = self.load_registry()?.live().len();
        if live > 0 && !force {
            return Err(format!(
                "confirmation_required: {live} agente(s) vivo(s) usan el bus; limpiarlo corta su comunicación"
            ));
        }
        let removed = self.bus()?.since(0).map_err(|e| e.to_string())?.len();
        let path = self.service_config.bus_path();
        let name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
        if let Some(dir) = path.parent() {
            for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&*name.to_string_lossy())
                {
                    std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
                }
            }
        }
        self.nudged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.topic_wake_notified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(removed)
    }

    /// Writes to service.log which code path marked a live agent as `exited`, so a bogus exit can be
    /// traced instead of guessed.
    fn record_exit(&self, id: &str) {
        let trace = std::backtrace::Backtrace::force_capture().to_string();
        let frames: Vec<&str> = trace
            .lines()
            .map(str::trim)
            .filter(|l| l.contains("shepherd_core::") || l.contains("shepherd::"))
            .filter(|l| !l.contains("record_exit") && !l.contains("save_registry"))
            .take(6)
            .collect();
        self.record_failure(
            "agent_exited",
            &format!("{id} marcado exited por: {}", frames.join(" <- ")),
        );
    }

    /// True when the agent's terminal answers any of three independent lookups. `not found` from one of
    /// them is not proof: the server answers it for panes it has just created while it is busy.
    pub(crate) fn terminal_alive(&self, agent: &Agent) -> bool {
        let by_target = self.terminals.agent_status(&status_query_target(agent));
        if by_target.as_ref().is_ok_and(|s| s.state != "finished") {
            return true;
        }
        if self
            .terminals
            .agent_status(&agent.id)
            .is_ok_and(|s| s.state != "finished")
        {
            return true;
        }
        !agent.pane.is_empty() && self.terminals.read_screen(&agent.pane, 1).is_ok()
    }

    /// Whether a run is waiting for its quiet period to end, so the caller should tick soon.
    pub fn wants_fast_tick(&self) -> bool {
        self.fast_tick.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn load_registry(&self) -> Result<AgentRegistryFile, String> {
        load_agent_registry(self.service_config.registry_path()).map_err(|e| e.to_string())
    }

    /// Saves the registry without losing what other threads wrote since `registry` was loaded: agents
    /// that are only on disk are kept and, for the rest, the more recently updated copy wins (the caller's on a tie). Several
    /// threads (spawning a pattern's agents, first-prompt delivery, the reconcile loop) read, change
    /// and save the whole file, and a stale snapshot used to drop freshly spawned agents.
    pub fn save_registry(&self, registry: &AgentRegistryFile) -> Result<(), String> {
        let _io = self.registry_io.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.service_config.registry_path();
        let removed: HashSet<String> = std::mem::take(
            &mut *self
                .removed_agents
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if let Ok(before) = load_agent_registry(path.clone()) {
            for a in &registry.agents {
                let was_live = before.result(&a.id).is_some_and(|b| !b.gone());
                if matches!(a.state.as_str(), "exited" | "failed" | "killed") && was_live {
                    self.record_exit(&a.id);
                }
            }
        }
        let mut merged = registry.clone();
        merged.agents.retain(|a| !removed.contains(&a.id));
        if let Ok(disk) = load_agent_registry(path.clone()) {
            for theirs in disk.agents {
                if removed.contains(&theirs.id) {
                    continue;
                }
                match merged.agents.iter_mut().find(|a| a.id == theirs.id) {
                    None => merged.agents.push(theirs),
                    // a stale snapshot must not undo a newer state (`starting` over `working`)
                    Some(mine) if theirs.updated > mine.updated => *mine = theirs,
                    Some(_) => {}
                }
            }
            merged.seq = merged.seq.max(disk.seq);
            for (role, n) in disk.next {
                let e = merged.next.entry(role).or_insert(0);
                *e = (*e).max(n);
            }
        }
        save_agent_registry(path, &merged).map_err(|e| e.to_string())
    }

    pub fn failed_requests(&self) -> Vec<FailedRequestLogEntry> {
        self.failed_requests.lock().unwrap().clone()
    }

    /// Requirement 8: Record every failed request in the service log (`service.log`, 0600) and in memory.
    pub fn record_failure(&self, method: &str, message: &str) {
        let entry = FailedRequestLogEntry {
            timestamp: self.clock.now_rfc3339(),
            method: method.to_owned(),
            message: message.to_owned(),
        };
        self.failed_requests.lock().unwrap().push(entry.clone());
        let log_path = self.service_config.log_path();
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let Ok(mut file) = options.open(&log_path) {
            let _ = writeln!(
                file,
                "[{}] method={} error={}",
                entry.timestamp, entry.method, entry.message
            );
        }
    }

    pub(crate) fn log_err<T>(&self, method: &str, result: Result<T, String>) -> Result<T, String> {
        if let Err(ref err) = result {
            self.record_failure(method, err);
        }
        result
    }

    pub fn list_public_agents(&self) -> Result<Vec<Agent>, String> {
        let result = (|| {
            let registry = self.load_registry()?;
            let mut agents: Vec<Agent> = registry.agents.iter().map(Agent::public).collect();
            self.annotate_snapshots(&mut agents);
            Ok(agents)
        })();
        self.log_err("agents", result)
    }

    /// List tasks in project root.
    pub fn list_tasks(&self, root: &Path, archived: bool) -> Result<Vec<CreatedTask>, String> {
        crate::list_tasks(root, archived).map_err(|e| e.to_string())
    }

    pub fn create_task(
        &self,
        root: &Path,
        title: String,
        description: String,
        criteria: Vec<String>,
        links: Vec<String>,
    ) -> Result<CreatedTask, String> {
        let new_task = crate::NewTask {
            id: 0,
            title,
            description,
            criteria,
            issue: None,
            issue_markdown: None,
            links,
        };
        crate::create_task(root, new_task).map_err(|e| e.to_string())
    }

    pub fn archive_task(&self, root: &Path, id: &str) -> Result<PathBuf, String> {
        crate::archive_task(root, id).map_err(|e| e.to_string())
    }

    pub fn start_run(
        &self,
        root: PathBuf,
        task: String,
        auto_push: bool,
        mcp_url: Option<String>,
    ) -> Result<(bool, String, String), String> {
        let job_id = format!("run-{}", task);
        let this = self.clone();
        let task_clone = task.clone();
        std::thread::Builder::new()
            .name(format!("shepherd-run-{}", task))
            .spawn(move || {
                let _ = this.run_workflow(root, task_clone, mcp_url, auto_push);
            })
            .map_err(|e| e.to_string())?;
        Ok((true, job_id, task))
    }

    pub fn list_runs(&self, root: &Path) -> Result<Vec<RunSummary>, String> {
        let tasks = crate::list_tasks(root, false).unwrap_or_default();
        let runs = tasks
            .into_iter()
            .map(|t| {
                let has_plan = t.directory.join("plan.md").is_file();
                let pending_gate = if t.status.waiting == "plan" {
                    Some(serde_json::json!("plan"))
                } else {
                    t.status
                        .subtasks
                        .iter()
                        .find(|s| s.state == "awaiting-approval")
                        .map(|subtask| {
                            serde_json::json!({
                                "kind": "push_pr",
                                "subtask": subtask.number,
                            })
                        })
                };
                let total = t.status.subtasks.len();
                let done_count = t
                    .status
                    .subtasks
                    .iter()
                    .filter(|s| matches!(s.state.as_str(), "pr" | "done"))
                    .count();
                let subtasks = t
                    .status
                    .subtasks
                    .iter()
                    .map(|s| RunSubtaskSummary {
                        n: s.number,
                        title: s.title.clone(),
                        state: s.state.clone(),
                        branch: s.branch.clone(),
                        pr: s.pr.clone(),
                        note: s.note.clone(),
                        coder: s.coder.clone(),
                        reviewer: s.reviewer.clone(),
                    })
                    .collect();
                RunSummary {
                    task_id: t.status.id,
                    task_title: t.status.title,
                    status: t.status.state.clone(),
                    current_stage: t.status.state,
                    has_saved_plan: has_plan,
                    pending_gate,
                    active_agents: t.status.agents,
                    subtasks_progress: [done_count, total],
                    subtasks,
                }
            })
            .collect();
        Ok(runs)
    }

    pub fn list_skills(&self, root: &Path) -> Result<Vec<skills::Skill>, String> {
        let cat = crate::skills::Catalog::load(Some(root), None);
        Ok(cat.all())
    }

    pub fn git_status(&self, root: &Path) -> Result<git::GitRepoStatus, String> {
        let registry = self.load_registry().unwrap_or_default();
        let base_worktrees = self.terminals.list_worktrees(root).ok();
        crate::git::collect_git_status_with_base(root, &registry.agents, base_worktrees)
            .map_err(|e| e.to_string())
    }

    pub fn list_patterns(
        &self,
        root: &Path,
    ) -> Result<Vec<crate::patterns::PatternSummary>, String> {
        crate::patterns::list_patterns(root, None).map_err(|e| e.to_string())
    }

    pub fn get_pattern(
        &self,
        root: &Path,
        name: &str,
    ) -> Result<crate::patterns::PatternDetail, String> {
        let (pattern, source, path) =
            crate::patterns::load_pattern(name, root, None).map_err(|e| e.to_string())?;
        let config = load_config(root).unwrap_or_default();
        let problems = crate::patterns::validate(&pattern, &config);
        Ok(crate::patterns::PatternDetail {
            pattern,
            source,
            path: path.map(|p| p.display().to_string()),
            problems,
        })
    }

    pub fn save_pattern(
        &self,
        root: &Path,
        pattern: &crate::patterns::Pattern,
        is_global: bool,
    ) -> Result<(PathBuf, Vec<crate::patterns::Problem>), String> {
        let config = load_config(root).unwrap_or_default();
        let problems = crate::patterns::validate(pattern, &config);
        let has_errors = problems
            .iter()
            .any(|p| p.severity == crate::patterns::Severity::Error);
        if has_errors {
            return Err(format!(
                "pattern validation failed: {}",
                serde_json::to_string(&problems).unwrap_or_default()
            ));
        }
        let path = crate::patterns::save_pattern(pattern, is_global, root, None)
            .map_err(|e| e.to_string())?;
        Ok((path, problems))
    }

    pub fn delete_pattern(
        &self,
        root: &Path,
        name: &str,
    ) -> Result<crate::patterns::PatternSource, String> {
        crate::patterns::delete_pattern(name, root, None).map_err(|e| e.to_string())
    }

    pub fn validate_pattern(
        &self,
        root: &Path,
        pattern: Option<&crate::patterns::Pattern>,
        name: Option<&str>,
    ) -> Result<(bool, Vec<crate::patterns::Problem>), String> {
        let config = load_config(root).unwrap_or_default();
        let pat = if let Some(p) = pattern {
            p.clone()
        } else if let Some(n) = name {
            let (loaded, _, _) =
                crate::patterns::load_pattern(n, root, None).map_err(|e| e.to_string())?;
            loaded
        } else {
            return Err("missing 'pattern' or 'name' for pattern_validate".to_owned());
        };
        let problems = crate::patterns::validate(&pat, &config);
        let valid = !problems
            .iter()
            .any(|p| p.severity == crate::patterns::Severity::Error);
        Ok((valid, problems))
    }

    pub fn list_topics(&self) -> Result<Vec<crate::bus::TopicSummary>, String> {
        let bus = self.bus()?;
        bus.list_topics().map_err(|e| e.to_string())
    }

    pub fn create_topic(&self, name: &str, partitions: u32) -> Result<crate::bus::Topic, String> {
        let bus = self.bus()?;
        bus.topic_create(name, partitions)
            .map_err(|e| e.to_string())
    }

    pub fn publish_topic(
        &self,
        topic: &str,
        content: &str,
        key: Option<&str>,
        partition: Option<u32>,
        from: &str,
    ) -> Result<crate::bus::Message, String> {
        let bus = self.bus()?;
        let msg = bus
            .publish(topic, content, key, partition, from)
            .map_err(|e| e.to_string())?;
        if let Ok(mut reg) = self.load_registry() {
            if let Err(e) = patterns_run::reopen_quiesced_runs(
                &self.service_config.state_dir,
                &reg,
                topic,
                &self.clock.now_rfc3339(),
            ) {
                self.record_failure("reopen_quiesced_runs", &e);
            }
            self.wake_dormant_topic_subscribers(&mut reg, Some(topic));
        }
        Ok(msg)
    }

    pub fn replay_topic(
        &self,
        topic: &str,
        partition: Option<u32>,
        from_offset: u64,
        limit: Option<usize>,
    ) -> Result<Vec<crate::bus::Message>, String> {
        let bus = self.bus()?;
        bus.replay(topic, partition, from_offset, limit)
            .map_err(|e| e.to_string())
    }

    pub fn consumer_groups(
        &self,
        topic: Option<&str>,
    ) -> Result<Vec<crate::bus::ConsumerGroupSummary>, String> {
        let bus = self.bus()?;
        bus.consumer_groups(topic).map_err(|e| e.to_string())
    }

    /// Generic dispatcher for `shepherd.call {op, args}`.
    pub fn handle_call(&self, op: &str, args: Value) -> Result<Value, String> {
        match op {
            "agents" => {
                let agents = self.list_public_agents()?;
                serde_json::to_value(agents).map_err(|e| e.to_string())
            }
            "spawn" => {
                let req: ServiceSpawnRequest = serde_json::from_value(args).map_err(|e| {
                    let msg = format!("invalid spawn args: {e}");
                    self.record_failure("spawn", &msg);
                    msg
                })?;
                let agent = self.spawn(req)?;
                serde_json::to_value(agent.public()).map_err(|e| e.to_string())
            }
            "kill" => {
                let id = args.get("id").and_then(Value::as_str).ok_or_else(|| {
                    let msg = "missing 'id' for kill".to_owned();
                    self.record_failure("kill", &msg);
                    msg
                })?;
                let agent = self.kill(id)?;
                serde_json::to_value(agent.public()).map_err(|e| e.to_string())
            }
            "clean" => {
                let ids: Vec<String> = args
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let workspace = args.get("workspace").and_then(Value::as_str);
                let cleaned = self.clean(&ids, workspace)?;
                Ok(serde_json::json!({ "cleaned": cleaned }))
            }
            "focus" => {
                let id = args.get("id").and_then(Value::as_str).ok_or_else(|| {
                    let msg = "missing 'id' for focus".to_owned();
                    self.record_failure("focus", &msg);
                    msg
                })?;
                let agent = self.focus(id)?;
                serde_json::to_value(agent).map_err(|e| e.to_string())
            }
            "run" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let task = args
                    .get("task")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        let msg = "missing 'task' for run".to_owned();
                        self.record_failure("run", &msg);
                        msg
                    })?
                    .to_owned();
                let mcp_url = args
                    .get("mcp_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let auto_push = args
                    .get("auto_push")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let (accepted, job_id, task) = self.start_run(root, task, auto_push, mcp_url)?;
                Ok(serde_json::json!({
                    "accepted": accepted,
                    "job": job_id,
                    "task": task,
                }))
            }
            "approve" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let task = args.get("task").and_then(Value::as_str).ok_or_else(|| {
                    let msg = "missing 'task' for approve".to_owned();
                    self.record_failure("approve", &msg);
                    msg
                })?;
                let target = args.get("target").and_then(Value::as_str).ok_or_else(|| {
                    let msg = "missing 'target' for approve".to_owned();
                    self.record_failure("approve", &msg);
                    msg
                })?;
                self.approve(root, task, target)
            }
            "reject" | "reject_gate" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let task = args.get("task").and_then(Value::as_str).ok_or_else(|| {
                    let msg = "missing 'task' for reject".to_owned();
                    self.record_failure("reject", &msg);
                    msg
                })?;
                let target = args.get("target").and_then(Value::as_str).ok_or_else(|| {
                    let msg = "missing 'target' for reject".to_owned();
                    self.record_failure("reject", &msg);
                    msg
                })?;
                let reason = args.get("reason").and_then(Value::as_str);
                self.reject(root, task, target, reason)
            }
            "reconcile" => {
                let public = self.reconcile_agents()?;
                serde_json::to_value(public).map_err(|e| e.to_string())
            }
            "resume" => {
                let public = self.resume_agents()?;
                serde_json::to_value(public).map_err(|e| e.to_string())
            }
            "tasks" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let archived = args
                    .get("archived")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let tasks = self.list_tasks(&root, archived)?;
                serde_json::to_value(tasks).map_err(|e| e.to_string())
            }
            "task_create" | "create_task" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let title = args
                    .get("title")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'title' for task_create".to_owned())?
                    .to_owned();
                let description = args
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let criteria: Vec<String> = args
                    .get("criteria")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let attachments: Vec<String> = args
                    .get("attachments")
                    .or_else(|| args.get("links"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let created = self.create_task(&root, title, description, criteria, attachments)?;
                serde_json::to_value(created).map_err(|e| e.to_string())
            }
            "task_archive" | "archive_task" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let id = args
                    .get("id")
                    .and_then(|v| {
                        v.as_u64()
                            .map(|n| n.to_string())
                            .or_else(|| v.as_str().map(str::to_owned))
                    })
                    .ok_or_else(|| "missing 'id' for task_archive".to_owned())?;
                let path = self.archive_task(&root, &id)?;
                Ok(serde_json::json!({ "archived": true, "path": path.display().to_string() }))
            }
            "skills" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let all = self.list_skills(&root)?;
                serde_json::to_value(all).map_err(|e| e.to_string())
            }
            "patterns" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let list = self.list_patterns(&root)?;
                serde_json::to_value(list).map_err(|e| e.to_string())
            }
            "pattern_get" => {
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'name' for pattern_get".to_owned())?;
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let detail = self.get_pattern(&root, name)?;
                serde_json::to_value(detail).map_err(|e| e.to_string())
            }
            "pattern_save" => {
                let pattern_val = args
                    .get("pattern")
                    .ok_or_else(|| "missing 'pattern' for pattern_save".to_owned())?;
                let pattern: crate::patterns::Pattern = serde_json::from_value(pattern_val.clone())
                    .map_err(|e| format!("invalid pattern json: {e}"))?;
                let is_global = args.get("global").and_then(Value::as_bool).unwrap_or(false);
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let (path, problems) = self.save_pattern(&root, &pattern, is_global)?;
                Ok(serde_json::json!({
                    "ok": true,
                    "name": pattern.name,
                    "path": path.display().to_string(),
                    "source": if is_global { "global" } else { "project" },
                    "problems": problems,
                }))
            }
            "pattern_delete" => {
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'name' for pattern_delete".to_owned())?;
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let source = self.delete_pattern(&root, name)?;
                Ok(serde_json::json!({
                    "ok": true,
                    "name": name,
                    "deleted_source": source.as_str(),
                }))
            }
            "pattern_validate" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let pattern = if let Some(pat_val) = args.get("pattern") {
                    Some(
                        serde_json::from_value::<crate::patterns::Pattern>(pat_val.clone())
                            .map_err(|e| format!("invalid pattern json: {e}"))?,
                    )
                } else {
                    None
                };
                let name = args.get("name").and_then(Value::as_str);
                let (valid, problems) = self.validate_pattern(&root, pattern.as_ref(), name)?;
                Ok(serde_json::json!({
                    "valid": valid,
                    "problems": problems,
                }))
            }
            "pattern_run_start" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let pattern_name = args
                    .get("pattern")
                    .or_else(|| args.get("name"))
                    .and_then(Value::as_str);
                let pattern_obj = if let Some(pat_val) = args.get("pattern_obj") {
                    Some(
                        serde_json::from_value::<crate::patterns::Pattern>(pat_val.clone())
                            .map_err(|e| format!("invalid pattern_obj json: {e}"))?,
                    )
                } else {
                    None
                };
                let task = args.get("task").and_then(Value::as_str);
                let worktree = args.get("worktree").and_then(Value::as_str);
                let confirm = args
                    .get("confirm_over_limit")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let workspace = args.get("workspace_id").and_then(Value::as_str);
                let run = self.start_pattern_run_in(
                    &root,
                    pattern_name,
                    pattern_obj,
                    task,
                    worktree,
                    confirm,
                    workspace,
                )?;
                serde_json::to_value(run).map_err(|e| e.to_string())
            }
            "runs_clean" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let removed = self.clean_pattern_runs(&root)?;
                Ok(serde_json::json!({ "removed": removed }))
            }
            "bus_clean" => {
                let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
                let removed = self.clean_bus(force)?;
                Ok(serde_json::json!({ "removed": removed }))
            }
            "pattern_runs" | "pattern_run_list" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let runs = self.list_pattern_runs(&root)?;
                serde_json::to_value(runs).map_err(|e| e.to_string())
            }
            "pattern_run_show" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'id' for pattern_run_show".to_owned())?;
                let run = self.get_pattern_run(&root, id)?;
                serde_json::to_value(run).map_err(|e| e.to_string())
            }
            "pattern_run_pause" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'id' for pattern_run_pause".to_owned())?;
                let run = self.pause_pattern_run(&root, id)?;
                serde_json::to_value(run).map_err(|e| e.to_string())
            }
            "pattern_run_resume" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'id' for pattern_run_resume".to_owned())?;
                let run = self.resume_pattern_run(&root, id)?;
                serde_json::to_value(run).map_err(|e| e.to_string())
            }
            "pattern_run_stop" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'id' for pattern_run_stop".to_owned())?;
                let confirm = args
                    .get("confirm")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let run = self.stop_pattern_run(&root, id, confirm)?;
                serde_json::to_value(run).map_err(|e| e.to_string())
            }
            "runs" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let runs = self.list_runs(&root)?;
                serde_json::to_value(runs).map_err(|e| e.to_string())
            }
            "git_status" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
                let status = self.git_status(&root)?;
                serde_json::to_value(status).map_err(|e| e.to_string())
            }
            "topics" => {
                let list = self.list_topics()?;
                serde_json::to_value(list).map_err(|e| e.to_string())
            }
            "topic_create" => {
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'name' for topic_create".to_owned())?;
                let partitions = args.get("partitions").and_then(Value::as_u64).unwrap_or(1) as u32;
                let topic = self.create_topic(name, partitions)?;
                serde_json::to_value(topic).map_err(|e| e.to_string())
            }
            "publish" => {
                let topic = args
                    .get("topic")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'topic' for publish".to_owned())?;
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'content' for publish".to_owned())?;
                let key = args.get("key").and_then(Value::as_str);
                let partition = args
                    .get("partition")
                    .and_then(Value::as_u64)
                    .map(|p| p as u32);
                let from = args.get("from").and_then(Value::as_str).unwrap_or("user");
                let msg = self.publish_topic(topic, content, key, partition, from)?;
                serde_json::to_value(msg).map_err(|e| e.to_string())
            }
            "topic_tail" | "topic_replay" => {
                let topic = args
                    .get("topic")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing 'topic' for topic_tail/replay".to_owned())?;
                let partition = args
                    .get("partition")
                    .and_then(Value::as_u64)
                    .map(|p| p as u32);
                let from_offset = args
                    .get("from_offset")
                    .or_else(|| args.get("from"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|l| l as usize);
                let msgs = self.replay_topic(topic, partition, from_offset, limit)?;
                serde_json::to_value(msgs).map_err(|e| e.to_string())
            }
            "consumer_groups" => {
                let topic = args.get("topic").and_then(Value::as_str);
                let groups = self.consumer_groups(topic)?;
                serde_json::to_value(groups).map_err(|e| e.to_string())
            }
            other => {
                let msg = format!("unknown shepherd operation {other:?}");
                self.record_failure(other, &msg);
                Err(msg)
            }
        }
    }
}

pub mod delivery;
pub mod gates;
pub(crate) mod pattern_prompt;
pub mod patterns_run;
pub mod reconcile;
pub mod registry;
pub mod snapshots;
pub mod spawn;

pub(crate) use registry::status_query_target;
pub use registry::{
    format_shep_content, format_shep_message, format_shep_sender, format_shep_topic_message,
    is_missing_terminal_error, validate_plain_agent_id, MAX_DELIVERED_MESSAGE_LEN,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_f11b;
#[cfg(test)]
mod tests_f12;
#[cfg(test)]
mod tests_f8f;
#[cfg(test)]
mod tests_f8g;
#[cfg(test)]
mod tests_f8h;
