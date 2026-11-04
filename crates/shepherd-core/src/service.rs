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
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bus::{Message, MessageBus, KIND_SYSTEM};
use crate::config::{self, available_vendors, load_config, resolve_role, Config};
use crate::skills;
use crate::spawn::{mcp_launch_args, role_prompt, RUNTIME_CONSTRAINTS};
use crate::terminals::{
    OpenAgentRequest, PromptOutcome, TerminalError, Terminals, WorktreeSpec,
};
use crate::{
    find_task, load_agent_registry, now_rfc3339, save_agent_registry, task_context, task_slug,
    Agent, AgentRegistryFile, SkillAdjust,
};

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

/// Deterministic test clock that never sleeps the OS thread.
#[derive(Clone, Default)]
pub struct ManualClock {
    now_ms: Arc<Mutex<u64>>,
    on_sleep: Arc<Mutex<Option<Arc<dyn Fn(u64) + Send + Sync>>>>,
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
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug)]
struct ScreenQuietTracker {
    last_screen: String,
    unchanged_since_ms: u64,
}

/// Core Shepherd orchestration service.
pub struct OrchestrationService {
    service_config: ServiceConfig,
    shepherd_config: Config,
    terminals: Arc<dyn Terminals>,
    clock: Arc<dyn Clock>,
    active_vendors_override: Option<Vec<String>>,
    mcp_url: Mutex<Option<String>>,
    blocked_announced: Mutex<HashSet<String>>,
    quiet_screens: Mutex<HashMap<String, ScreenQuietTracker>>,
    failed_requests: Mutex<Vec<FailedRequestLogEntry>>,
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
        Self {
            service_config: svc_cfg,
            shepherd_config,
            terminals,
            clock,
            active_vendors_override: None,
            mcp_url: Mutex::new(None),
            blocked_announced: Mutex::new(HashSet::new()),
            quiet_screens: Mutex::new(HashMap::new()),
            failed_requests: Mutex::new(Vec::new()),
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

    pub fn load_registry(&self) -> Result<AgentRegistryFile, String> {
        load_agent_registry(self.service_config.registry_path()).map_err(|e| e.to_string())
    }

    pub fn save_registry(&self, registry: &AgentRegistryFile) -> Result<(), String> {
        save_agent_registry(self.service_config.registry_path(), registry)
            .map_err(|e| e.to_string())
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

    fn log_err<T>(&self, method: &str, result: Result<T, String>) -> Result<T, String> {
        if let Err(ref err) = result {
            self.record_failure(method, err);
        }
        result
    }

    /// Requirement 4: Reconcile the persisted registry with the live runtime on startup (`resume`).
    /// - `starting` with missing pane -> `exited`
    /// - `starting` that is already working -> `working`
    /// - `starting` that never received its first prompt -> `failed` with relaunch instructions
    /// - Clear `notice` on all agents
    /// - Preserve `token`s (`los tokens sobreviven`)
    pub fn resume_on_startup(&self) -> Result<AgentRegistryFile, String> {
        let result = (|| {
            let mut registry = self.load_registry()?;
            let now = self.clock.now_rfc3339();
            for agent in &mut registry.agents {
                agent.notice.clear();
                if agent.gone() {
                    continue;
                }
                let target = status_query_target(agent);
                let live_status = if target.is_empty() {
                    None
                } else {
                    match self.terminals.agent_status(&target) {
                        Ok(st) if st.state != "finished" => Some(st),
                        Ok(_) => None,
                        Err(err) if is_missing_terminal_error(&err) => None,
                        Err(err) => return Err(err.to_string()),
                    }
                };

                if agent.state == "starting" {
                    match live_status {
                        None => {
                            agent.state = "exited".into();
                            agent.updated = now.clone();
                        }
                        Some(st) if st.status == "working" => {
                            agent.state = "working".into();
                            agent.updated = now.clone();
                        }
                        Some(_) => {
                            agent.state = "failed".into();
                            agent.error = format!(
                                "no recibió su primer prompt antes del reinicio; relánzalo con `shepherd shepherd spawn {} --vendor {}`",
                                agent.role, agent.vendor
                            );
                            agent.updated = now.clone();
                        }
                    }
                } else {
                    match live_status {
                        None => {
                            agent.state = "exited".into();
                            agent.updated = now.clone();
                        }
                        Some(st) => match st.status.as_str() {
                            "working" => {
                                agent.state = "working".into();
                                agent.updated = now.clone();
                            }
                            "blocked" => {
                                agent.state = "blocked".into();
                                agent.updated = now.clone();
                            }
                            "idle" | "done" => {
                                if agent.state != "done" {
                                    agent.state = "idle".into();
                                    agent.updated = now.clone();
                                }
                            }
                            _ => {}
                        },
                    }
                }
            }
            self.save_registry(&registry)?;
            Ok(registry)
        })();
        self.log_err("resume", result)
    }

    /// Requirement 2: Wait for the agent to accept input (up to `ready_timeout`, default 10m),
    /// deliver its first prompt, and verify that it reacted (`working` or `seq` advanced),
    /// resending up to `max_prompt_resends` (3) times before failing with a clear message.
    ///
    /// Sets `notice` (`BLOCKED_HUMAN_NOTICE`) if the terminal stays `blocked` for >= `notice_after`
    /// (3s default), ONLY for `blocked` (never for "not ready"). Clears `notice` on delivery,
    /// failure, or kill. Never answers a dialog on behalf of the human.
    pub fn deliver_first_prompt(&self, agent_id: &str, prompt: &str) -> Result<(), String> {
        let result = self.deliver_first_prompt_inner(agent_id, prompt);
        self.log_err("deliver_first_prompt", result)
    }

    fn deliver_first_prompt_inner(&self, agent_id: &str, prompt: &str) -> Result<(), String> {
        let start_ms = self.clock.now_ms();
        let ready_timeout_ms = self.service_config.ready_timeout.as_millis() as u64;
        let notice_after_ms = self.service_config.notice_after.as_millis() as u64;
        let mut blocked_since_ms: Option<u64> = None;

        // Phase 1: Wait until the agent is ready to accept input.
        let baseline_seq = loop {
            let mut registry = self.load_registry()?;
            let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) else {
                return Err(format!("unknown agent {agent_id:?}"));
            };
            if agent.gone() {
                let state = agent.state.clone();
                if !agent.notice.is_empty() {
                    agent.notice.clear();
                    self.save_registry(&registry)?;
                }
                return Err(format!(
                    "agent {agent_id} ended in state {state} before prompt delivery"
                ));
            }
            let target = status_query_target(agent);
            let now_ms = self.clock.now_ms();

            let status_res = self.terminals.agent_status(&target);
            match status_res {
                Ok(st) if st.state == "finished" => {
                    let err_msg = "el panel se cerró antes de entregar el primer prompt".to_owned();
                    agent.state = "exited".into();
                    agent.notice.clear();
                    agent.error = err_msg.clone();
                    agent.updated = self.clock.now_rfc3339();
                    self.save_registry(&registry)?;
                    return Err(err_msg);
                }
                Ok(st) => match st.status.as_str() {
                    "idle" | "done" | "working" => {
                        if !agent.notice.is_empty() {
                            agent.notice.clear();
                            self.save_registry(&registry)?;
                        }
                        break st.seq;
                    }
                    "blocked" => {
                        let since = *blocked_since_ms.get_or_insert(now_ms);
                        if now_ms.saturating_sub(since) >= notice_after_ms
                            && agent.notice != BLOCKED_HUMAN_NOTICE
                        {
                            agent.notice = BLOCKED_HUMAN_NOTICE.into();
                            agent.updated = self.clock.now_rfc3339();
                            self.save_registry(&registry)?;
                        }
                    }
                    _ => {
                        // Not ready (e.g. "starting", "unknown", "not_ready"):
                        // never set notice for "not ready", and clear if previously blocked.
                        blocked_since_ms = None;
                        if !agent.notice.is_empty() {
                            agent.notice.clear();
                            agent.updated = self.clock.now_rfc3339();
                            self.save_registry(&registry)?;
                        }
                    }
                },
                Err(err) if is_missing_terminal_error(&err) => {
                    let err_msg =
                        "el panel desapareció antes de entregar el primer prompt".to_owned();
                    agent.state = "exited".into();
                    agent.notice.clear();
                    agent.error = err_msg.clone();
                    agent.updated = self.clock.now_rfc3339();
                    self.save_registry(&registry)?;
                    return Err(err_msg);
                }
                Err(_) => {
                    blocked_since_ms = None;
                }
            }

            if now_ms.saturating_sub(start_ms) >= ready_timeout_ms {
                let mut registry = self.load_registry()?;
                if let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) {
                    agent.state = "failed".into();
                    agent.notice.clear();
                    agent.error = NEVER_READY_ERROR.into();
                    agent.updated = self.clock.now_rfc3339();
                    self.save_registry(&registry)?;
                }
                return Err(NEVER_READY_ERROR.to_owned());
            }

            self.clock.sleep(self.service_config.ready_poll_interval);
        };

        // Phase 2: Send prompt and verify reaction; resend up to `max_prompt_resends` (3) times.
        let total_sends = 1 + self.service_config.max_prompt_resends;
        let reaction_timeout_ms = self.service_config.reaction_timeout.as_millis() as u64;

        for attempt in 0..total_sends {
            let (prompt_target, status_target, is_raw_pane) = {
                let mut registry = self.load_registry()?;
                let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) else {
                    return Err(format!("unknown agent {agent_id:?}"));
                };
                if agent.gone() {
                    agent.notice.clear();
                    self.save_registry(&registry)?;
                    return Err(format!("agent {agent_id} was stopped during prompt delivery"));
                }
                let shepherd_agent = self
                    .shepherd_config
                    .vendors
                    .get(&agent.vendor)
                    .map(|v| v.shepherd_agent.as_str())
                    .unwrap_or("claude");
                let is_raw = shepherd_agent.is_empty();
                let p_target = if is_raw && !agent.pane.is_empty() {
                    agent.pane.clone()
                } else {
                    agent.id.clone()
                };
                (p_target, status_query_target(agent), is_raw)
            };

            let send_outcome = if is_raw_pane {
                self.terminals
                    .send_text(&prompt_target, prompt)
                    .map(|()| PromptOutcome::Delivered)
            } else {
                self.terminals.prompt(&prompt_target, prompt)
            };

            match send_outcome {
                Ok(PromptOutcome::Blocked) => {
                    // Terminal became blocked while sending; wait until ready or timeout.
                    let wait_res = self.wait_until_unblocked(agent_id, start_ms, ready_timeout_ms, notice_after_ms);
                    if let Err(err) = wait_res {
                        return Err(err);
                    }
                    continue;
                }
                Ok(PromptOutcome::Delivered) => {}
                Err(err) => {
                    if attempt + 1 == total_sends {
                        self.mark_prompt_failed(
                            agent_id,
                            &format!("no se pudo enviar el primer prompt tras {} reenvíos: {err}", self.service_config.max_prompt_resends),
                        )?;
                        return Err(format!("no se pudo enviar el primer prompt: {err}"));
                    }
                }
            }

            // Poll for reaction (status == "working" or seq > baseline_seq).
            let check_start_ms = self.clock.now_ms();
            loop {
                if let Ok(st) = self.terminals.agent_status(&status_target) {
                    if st.state != "finished" && (st.status == "working" || st.seq > baseline_seq) {
                        let mut registry = self.load_registry()?;
                        if let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) {
                            if !agent.gone() && agent.state != "done" {
                                agent.state = "working".into();
                            }
                            agent.notice.clear();
                            agent.error.clear();
                            agent.updated = self.clock.now_rfc3339();
                            self.save_registry(&registry)?;
                        }
                        return Ok(());
                    }
                }
                if self.clock.now_ms().saturating_sub(check_start_ms) >= reaction_timeout_ms {
                    break;
                }
                self.clock.sleep(self.service_config.reaction_poll_interval);
            }
        }

        let msg = format!(
            "el agente {agent_id} no reaccionó al primer prompt tras {} reenvíos",
            self.service_config.max_prompt_resends
        );
        self.mark_prompt_failed(agent_id, &msg)?;
        Err(msg)
    }

    fn wait_until_unblocked(
        &self,
        agent_id: &str,
        start_ms: u64,
        ready_timeout_ms: u64,
        notice_after_ms: u64,
    ) -> Result<(), String> {
        let blocked_since = self.clock.now_ms();
        loop {
            let mut registry = self.load_registry()?;
            let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) else {
                return Err(format!("unknown agent {agent_id:?}"));
            };
            if agent.gone() {
                let state = agent.state.clone();
                agent.notice.clear();
                self.save_registry(&registry)?;
                return Err(format!("agent {agent_id} ended in state {state}"));
            }
            let target = status_query_target(agent);
            let now_ms = self.clock.now_ms();
            let mut dirty = false;
            if now_ms.saturating_sub(blocked_since) >= notice_after_ms
                && agent.notice != BLOCKED_HUMAN_NOTICE
            {
                agent.notice = BLOCKED_HUMAN_NOTICE.into();
                agent.updated = self.clock.now_rfc3339();
                dirty = true;
            }
            if let Ok(st) = self.terminals.agent_status(&target) {
                if matches!(st.status.as_str(), "idle" | "done" | "working") {
                    if !agent.notice.is_empty() {
                        agent.notice.clear();
                        dirty = true;
                    }
                    if dirty {
                        self.save_registry(&registry)?;
                    }
                    return Ok(());
                }
            }
            if now_ms.saturating_sub(start_ms) >= ready_timeout_ms {
                agent.state = "failed".into();
                agent.notice.clear();
                agent.error = NEVER_READY_ERROR.into();
                agent.updated = self.clock.now_rfc3339();
                self.save_registry(&registry)?;
                return Err(NEVER_READY_ERROR.to_owned());
            }
            if dirty {
                self.save_registry(&registry)?;
            }
            self.clock.sleep(self.service_config.ready_poll_interval);
        }
    }

    fn mark_prompt_failed(&self, agent_id: &str, error_msg: &str) -> Result<(), String> {
        let mut registry = self.load_registry()?;
        if let Some(agent) = registry.agents.iter_mut().find(|a| a.id == agent_id) {
            agent.state = "failed".into();
            agent.notice.clear();
            agent.error = error_msg.to_owned();
            agent.updated = self.clock.now_rfc3339();
            self.save_registry(&registry)?;
        }
        Ok(())
    }

    /// Requirement 3: Periodic reconciliation against the live runtime.
    /// - Never touches `starting` or definitive states (`exited, killed, failed`).
    /// - Missing pane -> `exited`.
    /// - `working` -> `working`.
    /// - `blocked` -> `blocked` (announces ONCE per blocked episode in the bus timeline).
    /// - `idle`/`done` -> `idle` if agent has MCP, or `done` if non-MCP AND screen has been
    ///   unchanged for `>= non_mcp_done_quiet_secs` (capturing final screen as `result` if
    ///   `!agent.has_result`).
    pub fn reconcile_once(&self) -> Result<AgentRegistryFile, String> {
        let result = (|| {
            let mut registry = self.load_registry()?;
            let mut changed = false;
            let now_ms = self.clock.now_ms();
            let now_str = self.clock.now_rfc3339();
            let quiet_ms = self.service_config.non_mcp_done_quiet_secs.saturating_mul(1000);
            let bus = self.bus()?;

            for agent in &mut registry.agents {
                if agent.state == "starting" || agent.gone() {
                    continue;
                }
                let target = status_query_target(agent);
                let status = match self.terminals.agent_status(&target) {
                    Ok(st) if st.state != "finished" => st,
                    Ok(_) => {
                        agent.state = "exited".into();
                        agent.notice.clear();
                        agent.updated = now_str.clone();
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        changed = true;
                        continue;
                    }
                    Err(err) if is_missing_terminal_error(&err) => {
                        agent.state = "exited".into();
                        agent.notice.clear();
                        agent.updated = now_str.clone();
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        changed = true;
                        continue;
                    }
                    Err(err) => return Err(err.to_string()),
                };

                match status.status.as_str() {
                    "working" => {
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        if agent.state != "working" || !agent.notice.is_empty() {
                            agent.state = "working".into();
                            agent.notice.clear();
                            agent.updated = now_str.clone();
                            changed = true;
                        }
                    }
                    "blocked" => {
                        self.quiet_screens.lock().unwrap().remove(&agent.id);
                        if agent.state != "blocked" || agent.notice != BLOCKED_HUMAN_NOTICE {
                            agent.state = "blocked".into();
                            agent.notice = BLOCKED_HUMAN_NOTICE.into();
                            agent.updated = now_str.clone();
                            changed = true;
                        }
                        let newly_blocked = self
                            .blocked_announced
                            .lock()
                            .unwrap()
                            .insert(agent.id.clone());
                        if newly_blocked {
                            let _ = bus.send(Message {
                                id: 0,
                                ts: String::new(),
                                from: "system".into(),
                                to: "brain".into(),
                                kind: KIND_SYSTEM.into(),
                                content: format!(
                                    "agent {} ({}) is blocked waiting for human attention in pane {}",
                                    agent.id, agent.role, agent.pane
                                ),
                            });
                        }
                    }
                    "idle" | "done" => {
                        self.blocked_announced.lock().unwrap().remove(&agent.id);
                        if !agent.notice.is_empty() {
                            agent.notice.clear();
                            changed = true;
                        }
                        if agent.state == "blocked" {
                            agent.state = if agent.mcp { "idle".into() } else { "working".into() };
                            agent.updated = now_str.clone();
                            changed = true;
                        }
                        if agent.mcp {
                            if agent.state != "done" && agent.state != "idle" {
                                agent.state = "idle".into();
                                agent.updated = now_str.clone();
                                changed = true;
                            }
                        } else if agent.state != "done" {
                            let read_target = if !agent.pane.is_empty() {
                                agent.pane.as_str()
                            } else {
                                agent.id.as_str()
                            };
                            let screen = self
                                .terminals
                                .read_screen(read_target, 200)
                                .unwrap_or_default();
                            let is_quiet_enough = {
                                let mut trackers = self.quiet_screens.lock().unwrap();
                                let entry = trackers.entry(agent.id.clone()).or_insert_with(|| {
                                    ScreenQuietTracker {
                                        last_screen: screen.clone(),
                                        unchanged_since_ms: now_ms,
                                    }
                                });
                                if entry.last_screen != screen {
                                    entry.last_screen = screen.clone();
                                    entry.unchanged_since_ms = now_ms;
                                }
                                now_ms.saturating_sub(entry.unchanged_since_ms) >= quiet_ms
                            };
                            if is_quiet_enough {
                                agent.state = "done".into();
                                if !agent.has_result && agent.result.trim().is_empty() {
                                    let trimmed = screen.trim().to_owned();
                                    agent.result = trimmed;
                                    agent.has_result = !agent.result.is_empty();
                                }
                                agent.updated = now_str.clone();
                                self.quiet_screens.lock().unwrap().remove(&agent.id);
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }

            if changed {
                self.save_registry(&registry)?;
            }

            self.deliver_pending_non_mcp_messages(&mut registry);

            Ok(registry)
        })();
        self.log_err("reconcile", result)
    }

    /// Requirement 5 & 6: Spawn an agent with role/vendor/model routing, own worktree,
    /// `SHEP_AGENT_ID` + `SHEP_TOKEN` env, per-session skills, depth and concurrency
    /// limits (requiring confirmation before exceeding them), leftover-pane reclamation
    /// on name collision, and first-prompt delivery.
    pub fn spawn(&self, req: ServiceSpawnRequest) -> Result<Agent, String> {
        let result = self.spawn_inner(req);
        self.log_err("spawn", result)
    }

    fn spawn_inner(&self, req: ServiceSpawnRequest) -> Result<Agent, String> {
        if req.role.trim().is_empty() {
            return Err("role is required".into());
        }
        if req.task.trim().is_empty() {
            return Err("task prompt is required".into());
        }
        let project_cfg = if req.root.as_os_str().is_empty() {
            self.shepherd_config.clone()
        } else {
            load_config(&req.root).unwrap_or_else(|_| self.shepherd_config.clone())
        };
        let role_cfg = project_cfg
            .roles
            .get(&req.role)
            .ok_or_else(|| format!("unknown role {:?}", req.role))?
            .clone();

        let active_vendors = self
            .active_vendors_override
            .clone()
            .unwrap_or_else(|| available_vendors(&project_cfg));
        let (vendor_name, model) = match req.vendor.as_deref().filter(|v| !v.is_empty()) {
            Some(name) => {
                let vendor = project_cfg
                    .vendors
                    .get(name)
                    .ok_or_else(|| format!("unknown vendor {name:?}"))?;
                if vendor.disabled {
                    return Err(format!("vendor {name:?} is disabled"));
                }
                let model = req
                    .model
                    .clone()
                    .filter(|m| !m.is_empty())
                    .or_else(|| {
                        (!role_cfg.model.is_empty()
                            && (vendor.models.is_empty() || vendor.models.contains(&role_cfg.model)))
                        .then(|| role_cfg.model.clone())
                    })
                    .unwrap_or_else(|| vendor.default_model.clone());
                (name.to_owned(), model)
            }
            None => {
                let routed = resolve_role(&project_cfg, &active_vendors, &req.role)
                    .map_err(|e| e.to_string())?;
                let model = req
                    .model
                    .clone()
                    .filter(|m| !m.is_empty())
                    .unwrap_or(routed.model);
                (routed.vendor, model)
            }
        };
        let vendor = project_cfg
            .vendors
            .get(&vendor_name)
            .ok_or_else(|| format!("unknown vendor {vendor_name:?}"))?
            .clone();

        let mut registry = self.load_registry()?;
        let mut depth = 0;
        let parent_id = req.parent_id.clone().unwrap_or_default();
        if !parent_id.is_empty() {
            let parent = registry
                .agents
                .iter()
                .find(|a| a.id == parent_id)
                .ok_or_else(|| format!("unknown parent agent {parent_id:?}"))?;
            let parent_role = project_cfg
                .roles
                .get(&parent.role)
                .ok_or_else(|| format!("unknown parent role {:?}", parent.role))?;
            if !parent_role.can_spawn {
                return Err(format!(
                    "agent {parent_id} (role {}) is not allowed to spawn sub-agents",
                    parent.role
                ));
            }
            depth = parent.depth + 1;
            if depth > project_cfg.limits.max_spawn_depth && !req.confirm_over_limit {
                return Err(format!(
                    "confirmation_required: spawn depth {depth} exceeds max_spawn_depth ({}); pass confirm_over_limit to proceed",
                    project_cfg.limits.max_spawn_depth
                ));
            }
        }
        let live_count = registry.live().len();
        if live_count >= project_cfg.limits.max_concurrent_agents && !req.confirm_over_limit {
            return Err(format!(
                "confirmation_required: max_concurrent_agents ({}) reached ({live_count} live); pass confirm_over_limit to proceed",
                project_cfg.limits.max_concurrent_agents
            ));
        }

        let use_worktree = req.worktree.unwrap_or(role_cfg.worktree);
        let root_path = if req.root.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            req.root.clone()
        };
        let task_id = req.task_id.clone().unwrap_or_default();
        let mcp_url = req
            .mcp_url
            .clone()
            .filter(|u| !u.is_empty())
            .or_else(|| self.mcp_url());
        let mcp_enabled = vendor.mcp.supported && mcp_url.is_some();

        let now = self.clock.now_rfc3339();
        let short_slug = task_slug(if !task_id.is_empty() {
            &task_id
        } else {
            &req.task
        });
        let label = format!(
            "{}·{}·{}",
            req.role,
            vendor_name,
            &short_slug[..short_slug.len().min(16)]
        );
        let mut agent = registry
            .create_secure(
                Agent {
                    role: req.role.clone(),
                    vendor: vendor_name.clone(),
                    model: model.clone(),
                    task_id: task_id.clone(),
                    task_text: req.task.clone(),
                    parent_id,
                    depth,
                    root: root_path.display().to_string(),
                    mcp: mcp_enabled,
                    skills: req.skills.clone(),
                    label,
                    ..Agent::default()
                },
                &now,
            )
            .map_err(|e| e.to_string())?;

        let session_dir = self.service_config.agent_session_dir(&agent.id);
        let catalog = skills::Catalog::load(
            Some(&root_path),
            Some(&skills::user_skills_dir()),
        );
        let (_, langs) = skills::detect_checks(&root_path);
        let (resolved, _problems) =
            catalog.effective_for(&project_cfg, &req.role, &langs, &req.skills);
        agent.active_skills = resolved.iter().map(|r| r.skill.name.clone()).collect();
        let skill_launch = skills::install(skills::Plan {
            vendor: vendor.clone(),
            session_dir: session_dir.clone(),
            workdir: root_path.clone(),
            skills: resolved.into_iter().map(|r| r.skill).collect(),
            workspace: skills::workspace_instructions(&root_path),
            repo_docs: skills::repo_instruction_docs(&root_path),
        })?;

        let mut args = vendor.args.clone();
        let effective_model = if model.is_empty() {
            &vendor.default_model
        } else {
            &model
        };
        if !effective_model.is_empty() && !vendor.model_flag.is_empty() {
            args.extend([vendor.model_flag.clone(), effective_model.clone()]);
        }
        if let Some(ref url) = mcp_url {
            if vendor.mcp.supported {
                let mcp_args =
                    mcp_launch_args(&vendor, url, &agent.token).map_err(|e| e.to_string())?;
                args.extend(mcp_args);
            }
        }
        args.extend(skill_launch.args.clone());

        let mut env = HashMap::new();
        env.insert("SHEP_AGENT_ID".into(), agent.id.clone());
        env.insert("SHEP_ROLE".into(), req.role.clone());
        env.insert("SHEP_TOKEN".into(), agent.token.clone());
        if !task_id.is_empty() {
            env.insert("SHEP_TASK_ID".into(), task_id.clone());
        }

        let worktree = if use_worktree {
            let suffix = task_slug(if !task_id.is_empty() {
                &task_id
            } else {
                &req.task
            });
            let branch = if suffix.is_empty() {
                format!("shep/{}", agent.seq)
            } else {
                format!("shep/{}-{suffix}", agent.seq)
            };
            let base = (!project_cfg.workflow.base_branch.trim().is_empty())
                .then(|| project_cfg.workflow.base_branch.clone());
            Some(WorktreeSpec {
                branch,
                base,
            })
        } else {
            None
        };

        let open_req = OpenAgentRequest {
            root: root_path.clone(),
            role: req.role.clone(),
            agent_id: agent.id.clone(),
            vendor_name: vendor_name.clone(),
            command: vendor.command.clone(),
            shepherd_agent: vendor.shepherd_agent.clone(),
            args,
            label: agent.label.clone(),
            env,
            worktree,
        };

        // Requirement 6: Open agent, reclaiming a Shepherd leftover pane once if name collides.
        let opened = match self.open_or_reclaim_leftover(&open_req, &registry) {
            Ok(opened) => opened,
            Err(err) => {
                // Keep monotonic counters (`seq` and `next`) intact even when spawn fails!
                registry.remove(&agent.id);
                let _ = self.save_registry(&registry);
                return Err(err);
            }
        };

        agent.workspace = opened.workspace;
        agent.tab = opened.tab;
        agent.pane = opened.pane;
        agent.worktree = opened.worktree;
        agent.branch = opened.branch;
        agent.updated = self.clock.now_rfc3339();
        if let Some(slot) = registry.agents.iter_mut().find(|a| a.id == agent.id) {
            *slot = agent.clone();
        }
        self.save_registry(&registry)?;

        if req.deliver_prompt {
            let task_ctx = (!task_id.is_empty())
                .then(|| find_task(&root_path, &task_id).ok())
                .flatten()
                .map(|t| task_context(&t, 8000))
                .unwrap_or_default();
            let mut full_prompt = role_prompt(&root_path, &req.role, &role_cfg);
            full_prompt.push_str(&format!(
                "\n\nYour agent id is {} (role {}).",
                agent.id, agent.role
            ));
            if !agent.parent_id.is_empty() {
                full_prompt.push_str(&format!(" You were started by {}.", agent.parent_id));
            }
            if !agent.mcp {
                full_prompt.push_str(" The Shepherd MCP tools are not available yet; report your result in this terminal when finished.\n");
            } else {
                full_prompt.push('\n');
            }
            if !skill_launch.prompt.is_empty() {
                full_prompt.push_str("\n\n");
                full_prompt.push_str(&skill_launch.prompt);
            }
            if !task_ctx.is_empty() {
                full_prompt.push_str("\n\n");
                full_prompt.push_str(&task_ctx);
            }
            full_prompt.push_str(&format!("\n\n## Your task\n\n{}\n", req.task.trim()));
            if !agent.mcp {
                full_prompt.push('\n');
                full_prompt.push_str(RUNTIME_CONSTRAINTS);
            }
            self.deliver_first_prompt(&agent.id, &full_prompt)?;
            let refreshed = self.load_registry()?;
            if let Some(updated) = refreshed.result(&agent.id) {
                return Ok(updated.clone());
            }
        }

        Ok(agent)
    }

    /// Requirement 6: Unique agent names among open panes.
    /// If `open_agent` collides with an occupied name:
    /// - Check if the occupying pane is clearly a Shepherd leftover (label starts with `<role>·`
    ///   and no live registered agent in `registry` lives in it).
    /// - If so, close it and retry `open_agent` once.
    /// - Otherwise, NEVER close it and return an error explaining which pane/agent occupies it.
    fn open_or_reclaim_leftover(
        &self,
        open_req: &OpenAgentRequest,
        registry: &AgentRegistryFile,
    ) -> Result<crate::terminals::OpenedAgent, String> {
        if let Ok(existing) = self.terminals.agent_status(&open_req.agent_id) {
            if existing.state != "finished" {
                let expected_prefix = format!("{}·", open_req.role);
                let has_leftover_label = existing.title.starts_with(&expected_prefix);
                let occupied_by_live_registered = registry.live().iter().any(|live| {
                    live.id != open_req.agent_id
                        && ((!existing.pane.is_empty() && live.pane == existing.pane)
                            || (!existing.tab.is_empty() && live.tab == existing.tab)
                            || live.id == open_req.agent_id)
                }) || registry
                    .live()
                    .iter()
                    .any(|live| live.id == open_req.agent_id && !live.pane.is_empty());

                if has_leftover_label && !occupied_by_live_registered {
                    let close_target = if !existing.tab.is_empty() {
                        existing.tab.as_str()
                    } else if !existing.pane.is_empty() {
                        existing.pane.as_str()
                    } else {
                        open_req.agent_id.as_str()
                    };
                    self.terminals
                        .close(close_target)
                        .map_err(|e| format!("failed to close leftover pane {close_target}: {e}"))?;
                    return self
                        .terminals
                        .open_agent(open_req)
                        .map_err(|e| e.to_string());
                } else {
                    return Err(format!(
                        "el nombre de agente {:?} está ocupado por el panel {:?} (etiqueta {:?}); no es un resto de Shepherd y no se cerrará",
                        open_req.agent_id, existing.pane, existing.title
                    ));
                }
            }
        }
        self.terminals
            .open_agent(open_req)
            .map_err(|e| e.to_string())
    }

    /// Requirement 5 (`kill`):
    /// - Accepts live states (`starting, working, idle, blocked`) and `done`.
    /// - "Pane already closed" (`not_found`) counts as success.
    /// - Rejects already-definitive states (`exited, killed, failed`).
    pub fn kill(&self, id: &str) -> Result<Agent, String> {
        let result = (|| {
            let mut registry = self.load_registry()?;
            let Some(agent) = registry.agents.iter_mut().find(|a| a.id == id) else {
                return Err(format!("unknown agent {id:?}"));
            };
            if agent.gone() {
                return Err(format!(
                    "agent {id} is already in definitive state {:?}",
                    agent.state
                ));
            }
            let close_target = if !agent.tab.is_empty() {
                agent.tab.clone()
            } else if !agent.pane.is_empty() {
                agent.pane.clone()
            } else {
                agent.id.clone()
            };
            if !close_target.is_empty() {
                match self.terminals.close(&close_target) {
                    Ok(()) => {}
                    Err(err) if is_missing_terminal_error(&err) => {}
                    Err(err) => return Err(err.to_string()),
                }
            }
            agent.state = "killed".into();
            agent.notice.clear();
            agent.updated = self.clock.now_rfc3339();
            let updated = agent.clone();
            self.save_registry(&registry)?;
            Ok(updated)
        })();
        self.log_err("kill", result)
    }

    /// Requirement 5 (`clean`):
    /// - By explicit IDs or all definitive agents of the workspace.
    /// - Closes the pane of a `failed` agent that remained open.
    /// - Kills a `done` agent first if requested by explicit ID.
    /// - Rejects a running agent (`starting, working, idle, blocked`).
    /// - Deletes its session directory after validating that `id` is a plain name (no `/` or `..`).
    pub fn clean(&self, ids: &[String], workspace_root: Option<&str>) -> Result<Vec<String>, String> {
        let result = (|| {
            for id in ids {
                validate_plain_agent_id(id)?;
            }
            let mut registry = self.load_registry()?;
            let targets: Vec<String> = if !ids.is_empty() {
                for id in ids {
                    let Some(agent) = registry.agents.iter().find(|a| a.id == *id) else {
                        return Err(format!("unknown agent {id:?}"));
                    };
                    if matches!(
                        agent.state.as_str(),
                        "starting" | "working" | "idle" | "blocked"
                    ) {
                        return Err(format!(
                            "cannot clean running agent {id} in state {:?}",
                            agent.state
                        ));
                    }
                }
                ids.to_vec()
            } else {
                registry
                    .agents
                    .iter()
                    .filter(|agent| {
                        agent.gone()
                            && workspace_root
                                .filter(|w| !w.is_empty())
                                .is_none_or(|w| agent.root == w || agent.workspace == w)
                    })
                    .map(|agent| agent.id.clone())
                    .collect()
            };

            let mut cleaned = Vec::with_capacity(targets.len());
            for id in targets {
                validate_plain_agent_id(&id)?;
                if let Some(agent) = registry.agents.iter().find(|a| a.id == id).cloned() {
                    let close_target = if !agent.tab.is_empty() {
                        agent.tab.clone()
                    } else {
                        agent.pane.clone()
                    };
                    if (agent.state == "done" && !ids.is_empty()) || agent.state == "failed" {
                        if !close_target.is_empty() {
                            let _ = self.terminals.close(&close_target);
                        }
                    }
                    let session_dir = self.service_config.agent_session_dir(&id);
                    if session_dir.exists() {
                        std::fs::remove_dir_all(&session_dir)
                            .map_err(|e| format!("failed to remove session dir for {id}: {e}"))?;
                    }
                    registry.remove(&id);
                    cleaned.push(id);
                }
            }
            self.save_registry(&registry)?;
            Ok(cleaned)
        })();
        self.log_err("clean", result)
    }

    /// Requirement 5 (`focus`):
    /// Only selects/focuses the tab or pane in Shepherd's native UI (`agent.focus` / `tab.focus` / `pane.focus`).
    pub fn focus(&self, id: &str) -> Result<Agent, String> {
        let result = (|| {
            let registry = self.load_registry()?;
            let Some(agent) = registry.result(id).cloned() else {
                return Err(format!("unknown agent {id:?}"));
            };
            let primary = if !agent.pane.is_empty() {
                agent.pane.as_str()
            } else if !agent.tab.is_empty() {
                agent.tab.as_str()
            } else {
                agent.id.as_str()
            };
            if self.terminals.focus(&agent.id).is_err() {
                self.terminals
                    .focus(primary)
                    .map_err(|e| format!("failed to focus agent {id}: {e}"))?;
            }
            Ok(agent.public())
        })();
        self.log_err("focus", result)
    }

    /// Return public agent records (never exposing tokens).
    pub fn list_public_agents(&self) -> Result<Vec<Agent>, String> {
        let result = (|| {
            let registry = self.load_registry()?;
            Ok(registry.agents.iter().map(Agent::public).collect())
        })();
        self.log_err("agents", result)
    }

    /// Requirement 9: Run workflow with human approval gate by default, or auto-push when `auto_push` is true.
    pub fn run_workflow(
        &self,
        root: PathBuf,
        task: String,
        mcp_url: Option<String>,
        auto_push: bool,
    ) -> Result<Value, String> {
        let result = (|| {
            let effective_mcp_url = mcp_url
                .filter(|u| !u.is_empty())
                .or_else(|| self.mcp_url())
                .ok_or_else(|| "mcp_url is required when in-process MCP is not running".to_owned())?;
            let report = crate::workflow::run_coders_with_terminals(
                crate::workflow::RunRequest {
                    root: root.clone(),
                    task: task.clone(),
                    mcp_url: effective_mcp_url,
                },
                self.terminals.as_ref(),
                Some(self.service_config.registry_path()),
                self.active_vendors_override.clone(),
            )
            .map_err(|e| e.to_string())?;

            let mut pushed_prs = Vec::new();
            if auto_push && !report.plan_waiting {
                let loaded_task = find_task(&root, &task).map_err(|e| e.to_string())?;
                for subtask in &loaded_task.status.subtasks {
                    if subtask.state == "awaiting-approval" {
                        let pr_url = crate::workflow::approve_subtask_with_registry(
                            root.clone(),
                            &task,
                            subtask.number,
                            &self.service_config.registry_path(),
                        )
                        .map_err(|e| e.to_string())?;
                        pushed_prs.push(serde_json::json!({
                            "subtask": subtask.number,
                            "pr": pr_url,
                        }));
                    }
                }
            }

            Ok(serde_json::json!({
                "started": report.started,
                "completed_coding": report.completed_coding,
                "waiting_for_human": report.waiting_for_human,
                "failed": report.failed,
                "plan_waiting": report.plan_waiting,
                "auto_push": auto_push,
                "pushed_prs": pushed_prs,
            }))
        })();
        self.log_err("run", result)
    }

    /// Requirement 9: Explicit human approval for `plan` or a subtask number.
    pub fn approve(&self, root: PathBuf, task: &str, target: &str) -> Result<Value, String> {
        let result = (|| {
            if target == "plan" {
                crate::workflow::approve_plan(root, task).map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "task": task,
                    "approved": "plan",
                }))
            } else {
                let number: u32 = target
                    .parse()
                    .map_err(|_| format!("invalid approval target {target:?}; expected 'plan' or subtask number"))?;
                let pr_url = crate::workflow::approve_subtask_with_registry(
                    root,
                    task,
                    number,
                    &self.service_config.registry_path(),
                )
                .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "task": task,
                    "approved": number,
                    "pr": pr_url,
                }))
            }
        })();
        self.log_err("approve", result)
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
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
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
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
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
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
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
                self.run_workflow(root, task, mcp_url, auto_push)
            }
            "approve" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                let task = args
                    .get("task")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        let msg = "missing 'task' for approve".to_owned();
                        self.record_failure("approve", &msg);
                        msg
                    })?;
                let target = args
                    .get("target")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        let msg = "missing 'target' for approve".to_owned();
                        self.record_failure("approve", &msg);
                        msg
                    })?;
                self.approve(root, task, target)
            }
            "reconcile" => {
                let reg = self.reconcile_once()?;
                let public: Vec<Agent> = reg.agents.iter().map(Agent::public).collect();
                serde_json::to_value(public).map_err(|e| e.to_string())
            }
            "resume" => {
                let reg = self.resume_on_startup()?;
                let public: Vec<Agent> = reg.agents.iter().map(Agent::public).collect();
                serde_json::to_value(public).map_err(|e| e.to_string())
            }
            "tasks" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                let archived = args.get("archived").and_then(Value::as_bool).unwrap_or(false);
                let tasks = crate::list_tasks(&root, archived).map_err(|e| e.to_string())?;
                serde_json::to_value(tasks).map_err(|e| e.to_string())
            }
            "task_create" | "create_task" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
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
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default();
                let attachments: Vec<String> = args
                    .get("attachments")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default();
                let new_task = crate::NewTask {
                    id: 0,
                    title,
                    description,
                    criteria,
                    issue: None,
                    issue_markdown: None,
                    links: attachments,
                };
                let created = crate::create_task(&root, new_task).map_err(|e| e.to_string())?;
                serde_json::to_value(created).map_err(|e| e.to_string())
            }
            "task_archive" | "archive_task" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                let id = args
                    .get("id")
                    .and_then(|v| v.as_u64().map(|n| n.to_string()).or_else(|| v.as_str().map(str::to_owned)))
                    .ok_or_else(|| "missing 'id' for task_archive".to_owned())?;
                let path = crate::archive_task(&root, &id).map_err(|e| e.to_string())?;
                Ok(serde_json::json!({ "archived": true, "path": path.display().to_string() }))
            }
            "skills" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                let cat = crate::skills::Catalog::load(Some(&root), None);
                let all = cat.all();
                serde_json::to_value(all).map_err(|e| e.to_string())
            }
            "runs" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                let tasks = crate::list_tasks(&root, false).unwrap_or_default();
                let runs: Vec<serde_json::Value> = tasks
                    .into_iter()
                    .map(|t| {
                        let has_plan = t.directory.join("plan.md").is_file();
                        let pending_gate = if t.status.state == "awaiting-approval" {
                            Some("approve_plan")
                        } else {
                            None
                        };
                        serde_json::json!({
                            "task_id": t.status.id,
                            "task_title": t.status.title,
                            "status": t.status.state,
                            "current_stage": t.status.state,
                            "has_saved_plan": has_plan,
                            "pending_gate": pending_gate,
                            "active_agents": t.status.agents,
                            "subtasks_progress": [0, 0]
                        })
                    })
                    .collect();
                Ok(serde_json::json!(runs))
            }
            "git_status" => {
                let root = args
                    .get("root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                let registry = self.load_registry().unwrap_or_default();
                let status = crate::git::collect_git_status(&root, &registry.agents)
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(status).map_err(|e| e.to_string())
            }
            other => {
                let msg = format!("unknown shepherd operation {other:?}");
                self.record_failure(other, &msg);
                Err(msg)
            }
        }
    }

    fn deliver_pending_non_mcp_messages(&self, registry: &mut AgentRegistryFile) {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => {
                self.record_failure(
                    "deliver_bus_messages",
                    &format!("cannot open message bus: {e}"),
                );
                return;
            }
        };

        let mut registry_dirty = false;
        let reg_snapshot = registry.clone();

        for agent in &mut registry.agents {
            // Only live non-MCP agents (not mcp, not gone, not done, not starting)
            if agent.mcp || agent.gone() || agent.state == "done" || agent.state == "starting" {
                continue;
            }

            let messages = match bus.since(agent.delivery_cursor) {
                Ok(all) => {
                    let mut msgs: Vec<Message> = all
                        .into_iter()
                        .filter(|m| m.to == agent.id && m.kind != KIND_SYSTEM)
                        .collect();
                    msgs.sort_by_key(|m| m.id);
                    msgs
                }
                Err(e) => {
                    self.record_failure(
                        "deliver_bus_messages",
                        &format!(
                            "cannot read bus since {} for agent {}: {e}",
                            agent.delivery_cursor, agent.id
                        ),
                    );
                    continue;
                }
            };

            if messages.is_empty() {
                continue;
            }

            // Only type when pane is free (idle and quiet screen, not blocked, not working)
            if !self.is_pane_free_for_delivery(agent) {
                continue;
            }

            let target = if !agent.pane.is_empty() {
                agent.pane.clone()
            } else {
                agent.id.clone()
            };

            for msg in messages {
                if !self.is_pane_free_for_delivery(agent) {
                    break;
                }

                let line = format_shep_message(&msg.from, &msg.content, &reg_snapshot);
                let delivered =
                    self.deliver_single_message_with_retries(agent, &target, msg.id, &line);
                if delivered {
                    agent.delivery_cursor = msg.id;
                    agent.updated = self.clock.now_rfc3339();
                    registry_dirty = true;
                } else {
                    break;
                }
            }
        }

        if registry_dirty {
            if let Err(e) = self.save_registry(registry) {
                self.record_failure(
                    "deliver_bus_messages",
                    &format!("cannot save registry after message delivery: {e}"),
                );
            }
        }
    }

    fn is_pane_free_for_delivery(&self, agent: &Agent) -> bool {
        if !agent.notice.is_empty() || agent.state == "blocked" {
            return false;
        }

        let st = if !agent.id.is_empty() {
            if let Ok(st) = self.terminals.agent_status(&agent.id) {
                if st.status != "unknown" {
                    Some(st)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let status = match st {
            Some(s) => s,
            None => {
                let target = if !agent.pane.is_empty() {
                    &agent.pane
                } else {
                    &agent.id
                };
                match self.terminals.agent_status(target) {
                    Ok(s) => s,
                    Err(_) => return false,
                }
            }
        };

        if status.state == "finished" || status.status != "idle" {
            return false;
        }

        let s1 = self.read_screen_best_effort(agent);
        self.clock.sleep(Duration::from_millis(50));
        let s2 = self.read_screen_best_effort(agent);

        s1 == s2
    }

    fn read_screen_best_effort(&self, agent: &Agent) -> String {
        let targets = [&agent.pane, &agent.id];
        for t in targets {
            if !t.is_empty() {
                if let Ok(screen) = self.terminals.read_screen(t, 200) {
                    if !screen.is_empty() {
                        return screen;
                    }
                }
            }
        }
        for t in targets {
            if !t.is_empty() {
                if let Ok(screen) = self.terminals.read_screen(t, 200) {
                    return screen;
                }
            }
        }
        String::new()
    }

    fn deliver_single_message_with_retries(
        &self,
        agent: &Agent,
        target: &str,
        msg_id: i64,
        line: &str,
    ) -> bool {
        let max_retries = self.service_config.max_prompt_resends;
        let total_attempts = 1 + max_retries;
        let reaction_timeout_ms = self.service_config.reaction_timeout.as_millis() as u64;
        let poll_interval = self.service_config.reaction_poll_interval;

        for attempt in 0..total_attempts {
            if let Err(e) = self.terminals.send_text(target, line) {
                if attempt + 1 == total_attempts {
                    self.record_failure(
                        "deliver_bus_messages",
                        &format!(
                            "cannot send text to {target} for message {msg_id} after {max_retries} retries: {e}"
                        ),
                    );
                }
                continue;
            }

            let check_start_ms = self.clock.now_ms();
            let mut verified = false;
            let normalized_line: String = line.split_whitespace().collect::<Vec<_>>().join(" ");

            loop {
                let screen = self.read_screen_best_effort(agent);
                if screen.contains(line) {
                    verified = true;
                    break;
                }
                let normalized_screen: String =
                    screen.split_whitespace().collect::<Vec<_>>().join(" ");
                if normalized_screen.contains(&normalized_line) {
                    verified = true;
                    break;
                }

                if self.clock.now_ms().saturating_sub(check_start_ms) >= reaction_timeout_ms {
                    break;
                }
                self.clock.sleep(poll_interval);
            }

            if verified {
                return true;
            }
        }

        self.record_failure(
            "deliver_bus_messages",
            &format!(
                "message {msg_id} delivered to agent {} in pane {target} was not verified on screen after {max_retries} retries",
                agent.id
            ),
        );
        false
    }
}

pub const MAX_DELIVERED_MESSAGE_LEN: usize = 500;

pub fn format_shep_sender(from: &str, registry: &AgentRegistryFile) -> String {
    if let Some(agent) = registry.agents.iter().find(|a| a.id == from) {
        if let Some((role, num)) = agent.id.rsplit_once('-') {
            if !role.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                return format!("{role}#{num}");
            }
        }
        return format!("{}#{}", agent.role, agent.seq);
    }
    if let Some((role, num)) = from.rsplit_once('-') {
        if !role.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            return format!("{role}#{num}");
        }
    }
    from.to_owned()
}

pub fn format_shep_content(content: &str) -> String {
    let replaced = content
        .replace("\r\n", " ")
        .replace('\n', " ")
        .replace('\r', " ");
    let char_count = replaced.chars().count();
    if char_count > MAX_DELIVERED_MESSAGE_LEN {
        let truncated: String = replaced.chars().take(MAX_DELIVERED_MESSAGE_LEN).collect();
        format!("{truncated}…")
    } else {
        replaced
    }
}

pub fn format_shep_message(from: &str, content: &str, registry: &AgentRegistryFile) -> String {
    let sender = format_shep_sender(from, registry);
    let clean_content = format_shep_content(content);
    format!("[shep] from {sender}: {clean_content}")
}

fn status_query_target(agent: &Agent) -> String {
    if !agent.pane.is_empty() {
        agent.pane.clone()
    } else {
        agent.id.clone()
    }
}

pub fn is_missing_terminal_error(err: &TerminalError) -> bool {
    match err {
        TerminalError::Api { code, message } => {
            matches!(
                code.as_str(),
                "not_found" | "agent_not_found" | "pane_not_found" | "tab_not_found"
            ) || message.contains("not found")
        }
        TerminalError::Protocol(msg) => msg.contains("not found"),
        TerminalError::Io(io) => io.kind() == std::io::ErrorKind::NotFound,
        TerminalError::Config(_) => false,
    }
}

pub fn validate_plain_agent_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains('\0')
    {
        return Err(format!(
            "invalid agent id {id:?}: must be a plain name without '/' or '..'"
        ));
    }
    let mut components = Path::new(id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(format!(
            "invalid agent id {id:?}: must be a single normal path component"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminals::{InMemoryTerminals, TerminalAgentStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(1);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "shep-service-test-{name}-{}-{n}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_service(
        dir: &Path,
    ) -> (
        OrchestrationService,
        InMemoryTerminals,
        ManualClock,
    ) {
        let state_dir = dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let terminals = InMemoryTerminals::new();
        let clock = ManualClock::new();
        let svc_cfg = ServiceConfig::for_state_dir(&state_dir);
        let shep_cfg = Config::default();
        let service = OrchestrationService::new(
            svc_cfg,
            shep_cfg,
            Arc::new(terminals.clone()),
            Arc::new(clock.clone()),
        )
        .with_active_vendors(vec!["claude".into(), "codex".into(), "gemini".into()]);
        (service, terminals, clock)
    }

    #[test]
    fn req1_durable_registry_permissions_legacy_and_monotonic_counters() {
        let tmp = TestDir::new("test");
        let reg_path = tmp.path().join("agents.json");

        // 1. Load legacy bare array format
        let legacy_json = r#"[
            {
                "id": "coder-3",
                "seq": 7,
                "role": "coder",
                "vendor": "codex",
                "root": "/repo",
                "state": "done",
                "notice": "old notice",
                "token": "tok-3",
                "started": "2026-04-20T00:00:00Z",
                "updated": "2026-04-20T00:00:00Z"
            }
        ]"#;
        std::fs::write(&reg_path, legacy_json).unwrap();

        let mut reg = load_agent_registry(&reg_path).unwrap();
        assert_eq!(reg.seq, 7);
        assert_eq!(reg.next.get("coder"), Some(&3));
        assert_eq!(reg.agents[0].notice, "old notice");
        assert!(reg.agents[0].terminal());
        assert!(!reg.agents[0].gone());

        // 2. Remove coder-3, save, and reload: counters must NEVER go backwards
        assert!(reg.remove("coder-3"));
        save_agent_registry(&reg_path, &reg).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&reg_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "agents.json must be written with 0600 permissions");
        }

        let mut reloaded = load_agent_registry(&reg_path).unwrap();
        assert!(reloaded.agents.is_empty());
        assert_eq!(reloaded.seq, 7);
        assert_eq!(reloaded.next.get("coder"), Some(&3));

        let next_coder = reloaded
            .create_secure(
                Agent {
                    role: "coder".into(),
                    vendor: "codex".into(),
                    root: "/repo".into(),
                    ..Agent::default()
                },
                "2026-04-20T00:01:00Z",
            )
            .unwrap();
        assert_eq!(next_coder.id, "coder-4");
        assert_eq!(next_coder.seq, 8);
        assert_eq!(next_coder.state, "starting");
    }

    #[test]
    fn req2_first_prompt_delivery_blocked_notice_only_for_blocked_and_clears_on_delivery() {
        let tmp = TestDir::new("test");
        let (service, terminals, clock) = test_service(tmp.path());

        // Spawn without auto-delivering so we can step through readiness states
        let spawned = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "brain".into(),
                task: "plan feature".into(),
                deliver_prompt: false,
                ..ServiceSpawnRequest::default()
            })
            .unwrap();

        // Initially set status to "not_ready" for 5 seconds, then "blocked" for 4 seconds, then "idle"
        terminals.set_status(
            &spawned.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "not_ready".into(),
                pane: spawned.pane.clone(),
                tab: spawned.tab.clone(),
                title: spawned.label.clone(),
                seq: 1,
            },
        );

        let service_ref_path = service.service_config().registry_path();
        let terminals_hook = terminals.clone();
        let agent_id = spawned.id.clone();
        let pane_id = spawned.pane.clone();
        let tab_id = spawned.tab.clone();
        let label = spawned.label.clone();

        let not_ready_notice_seen = Arc::new(Mutex::new(None));
        let blocked_notice_seen = Arc::new(Mutex::new(None));
        let nr_clone = not_ready_notice_seen.clone();
        let bl_clone = blocked_notice_seen.clone();

        clock.set_on_sleep(move |now_ms| {
            if now_ms == 4_000 {
                // After 4s of "not_ready" (> 3s notice_after), notice MUST still be empty!
                let reg = load_agent_registry(&service_ref_path).unwrap();
                let a = reg.result(&agent_id).unwrap();
                *nr_clone.lock().unwrap() = Some(a.notice.clone());
                // Now switch to "blocked"
                terminals_hook.set_status(
                    &agent_id,
                    TerminalAgentStatus {
                        state: "live".into(),
                        status: "blocked".into(),
                        pane: pane_id.clone(),
                        tab: tab_id.clone(),
                        title: label.clone(),
                        seq: 1,
                    },
                );
            } else if now_ms == 7_400 {
                // 3.4s after becoming "blocked" (>= 3s notice_after): notice MUST be set!
                let reg = load_agent_registry(&service_ref_path).unwrap();
                let a = reg.result(&agent_id).unwrap();
                *bl_clone.lock().unwrap() = Some(a.notice.clone());
                // Human resolves dialog -> agent becomes "idle"
                terminals_hook.set_status(
                    &agent_id,
                    TerminalAgentStatus {
                        state: "live".into(),
                        status: "idle".into(),
                        pane: pane_id.clone(),
                        tab: tab_id.clone(),
                        title: label.clone(),
                        seq: 2,
                    },
                );
            }
        });

        service
            .deliver_first_prompt(&spawned.id, "hello world")
            .unwrap();

        assert_eq!(
            not_ready_notice_seen.lock().unwrap().as_deref(),
            Some(""),
            "notice must NEVER be set for not_ready"
        );
        assert_eq!(
            blocked_notice_seen.lock().unwrap().as_deref(),
            Some(BLOCKED_HUMAN_NOTICE),
            "notice must be set after notice_after in blocked state"
        );
        // Never answered any dialog on behalf of the human
        assert!(terminals.sent_keys().is_empty());
        assert!(terminals.sent_text().is_empty());

        // Notice must be cleared and state must be "working" after delivery
        let final_reg = service.load_registry().unwrap();
        let final_agent = final_reg.result(&spawned.id).unwrap();
        assert_eq!(final_agent.state, "working");
        assert!(final_agent.notice.is_empty());
    }

    #[test]
    fn req2_first_prompt_timeout_sets_never_ready_error_and_clears_notice() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());

        let spawned = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "brain".into(),
                task: "plan feature".into(),
                deliver_prompt: false,
                ..ServiceSpawnRequest::default()
            })
            .unwrap();

        terminals.set_status(
            &spawned.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "blocked".into(),
                pane: spawned.pane.clone(),
                tab: spawned.tab.clone(),
                title: spawned.label.clone(),
                seq: 1,
            },
        );

        let err = service
            .deliver_first_prompt(&spawned.id, "hello")
            .unwrap_err();
        assert_eq!(err, NEVER_READY_ERROR);

        let reg = service.load_registry().unwrap();
        let agent = reg.result(&spawned.id).unwrap();
        assert_eq!(agent.state, "failed");
        assert_eq!(agent.error, NEVER_READY_ERROR);
        assert!(
            agent.notice.is_empty(),
            "notice must be cleared upon failure"
        );
        assert!(terminals.sent_keys().is_empty());
    }

    #[test]
    fn req2_first_prompt_resends_up_to_3_times_when_agent_does_not_react() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());
        terminals.set_auto_react_on_prompt(false);

        let spawned = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "brain".into(),
                task: "plan feature".into(),
                deliver_prompt: false,
                ..ServiceSpawnRequest::default()
            })
            .unwrap();

        let err = service
            .deliver_first_prompt(&spawned.id, "do work")
            .unwrap_err();
        assert!(
            err.contains("no reaccionó al primer prompt tras 3 reenvíos"),
            "unexpected error: {err}"
        );
        // 1 initial send + 3 resends = 4 prompts sent
        assert_eq!(terminals.prompts().len(), 4);

        let reg = service.load_registry().unwrap();
        let agent = reg.result(&spawned.id).unwrap();
        assert_eq!(agent.state, "failed");
        assert!(agent.notice.is_empty());

        // Now test succeeding on the 2nd resend
        let spawned2 = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "brain".into(),
                task: "plan second feature".into(),
                deliver_prompt: false,
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        let terminals_clone = terminals.clone();
        let id2 = spawned2.id.clone();
        let pane2 = spawned2.pane.clone();
        let tab2 = spawned2.tab.clone();
        let label2 = spawned2.label.clone();
        terminals.on_prompt(move |_target, _text| {
            let n = count_clone.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 3 {
                // React on the 2nd resend (3rd send) by advancing seq
                terminals_clone.set_status(
                    &id2,
                    TerminalAgentStatus {
                        state: "live".into(),
                        status: "idle".into(),
                        pane: pane2.clone(),
                        tab: tab2.clone(),
                        title: label2.clone(),
                        seq: 99,
                    },
                );
            }
        });
        service
            .deliver_first_prompt(&spawned2.id, "do second work")
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 3);
        let reg2 = service.load_registry().unwrap();
        assert_eq!(reg2.result(&spawned2.id).unwrap().state, "working");
    }

    #[test]
    fn req3_periodic_reconciliation_blocked_once_and_non_mcp_quiet_screen_done() {
        let tmp = TestDir::new("test");
        let (service, terminals, clock) = test_service(tmp.path());
        service.set_mcp_url(Some("http://127.0.0.1:4317/mcp".into()));

        // 1. MCP agent (brain -> claude) and non-MCP agent (docs -> gemini/agy)
        let mcp_agent = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "brain".into(),
                task: "coordinate".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        assert!(mcp_agent.mcp);

        let non_mcp_agent = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "docs".into(),
                task: "summarize".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        assert!(!non_mcp_agent.mcp);

        // 2. Put MCP agent in "blocked" across two reconcile ticks -> bus message emitted ONLY ONCE
        terminals.set_status(
            &mcp_agent.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "blocked".into(),
                pane: mcp_agent.pane.clone(),
                tab: mcp_agent.tab.clone(),
                title: mcp_agent.label.clone(),
                seq: 3,
            },
        );
        service.reconcile_once().unwrap();
        service.reconcile_once().unwrap();

        let timeline = service.bus().unwrap().since(0).unwrap();
        let blocked_msgs: Vec<_> = timeline
            .iter()
            .filter(|m| m.content.contains(&mcp_agent.id) && m.content.contains("blocked"))
            .collect();
        assert_eq!(
            blocked_msgs.len(),
            1,
            "blocked state must be announced only once per episode on the bus"
        );

        // MCP agent becomes "idle" -> stays "idle" (not "done", because MCP agents finish via submit_result)
        terminals.set_status(
            &mcp_agent.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "idle".into(),
                pane: mcp_agent.pane.clone(),
                tab: mcp_agent.tab.clone(),
                title: mcp_agent.label.clone(),
                seq: 4,
            },
        );
        let reg = service.reconcile_once().unwrap();
        assert_eq!(reg.result(&mcp_agent.id).unwrap().state, "idle");

        // 3. Non-MCP agent (agy): Shepherd reports "done" prematurely while screen is still changing!
        terminals.set_status(
            &non_mcp_agent.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "done".into(),
                pane: non_mcp_agent.pane.clone(),
                tab: non_mcp_agent.tab.clone(),
                title: non_mcp_agent.label.clone(),
                seq: 5,
            },
        );
        terminals.set_screen(&non_mcp_agent.pane, "Thinking step 1...");
        let reg = service.reconcile_once().unwrap();
        assert_ne!(
            reg.result(&non_mcp_agent.id).unwrap().state,
            "done",
            "non-MCP agent must not be marked done before screen is quiet for 60s"
        );

        // Advance 30s, screen changes -> timer resets!
        clock.advance(Duration::from_secs(30));
        terminals.set_screen(&non_mcp_agent.pane, "Final summary output");
        let reg = service.reconcile_once().unwrap();
        assert_ne!(reg.result(&non_mcp_agent.id).unwrap().state, "done");

        // Advance 59s -> still < 60s since last screen change
        clock.advance(Duration::from_secs(59));
        let reg = service.reconcile_once().unwrap();
        assert_ne!(reg.result(&non_mcp_agent.id).unwrap().state, "done");

        // Advance 1 more second (total 60s unchanged) -> now marked "done" with screen captured as result!
        clock.advance(Duration::from_secs(1));
        let reg = service.reconcile_once().unwrap();
        let done_agent = reg.result(&non_mcp_agent.id).unwrap();
        assert_eq!(done_agent.state, "done");
        assert!(done_agent.has_result);
        assert_eq!(done_agent.result, "Final summary output");

        // 4. Missing pane -> transitions to "exited"
        terminals.remove_status(&mcp_agent.id);
        let reg = service.reconcile_once().unwrap();
        assert_eq!(reg.result(&mcp_agent.id).unwrap().state, "exited");
    }

    #[test]
    fn req4_resume_on_startup_reconciles_starting_and_preserves_tokens() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());

        let mut reg = AgentRegistryFile::default();
        // 1. starting without pane -> exited
        let mut a1 = reg.create(
            Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                pane: "pane-missing".into(),
                notice: "stale notice".into(),
                ..Agent::default()
            },
            "token-1".into(),
            "2026-04-20T00:00:00Z",
        );
        a1.pane = "pane-missing".into();
        reg.agents[0] = a1;

        // 2. starting that is already working -> working
        let mut a2 = reg.create(
            Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                pane: "pane-working".into(),
                notice: "stale notice 2".into(),
                ..Agent::default()
            },
            "token-2".into(),
            "2026-04-20T00:00:00Z",
        );
        a2.pane = "pane-working".into();
        reg.agents[1] = a2.clone();
        terminals.set_status(
            &a2.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "working".into(),
                pane: "pane-working".into(),
                tab: "tab-working".into(),
                title: "coder·codex·2".into(),
                seq: 2,
            },
        );

        // 3. starting that never received its first prompt (still idle) -> failed with relaunch instructions
        let mut a3 = reg.create(
            Agent {
                role: "reviewer".into(),
                vendor: "gemini".into(),
                pane: "pane-idle".into(),
                notice: "stale notice 3".into(),
                ..Agent::default()
            },
            "token-3".into(),
            "2026-04-20T00:00:00Z",
        );
        a3.pane = "pane-idle".into();
        reg.agents[2] = a3.clone();
        terminals.set_status(
            &a3.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "idle".into(),
                pane: "pane-idle".into(),
                tab: "tab-idle".into(),
                title: "reviewer·gemini·1".into(),
                seq: 1,
            },
        );

        service.save_registry(&reg).unwrap();
        let resumed = service.resume_on_startup().unwrap();

        let r1 = resumed.result("coder-1").unwrap();
        assert_eq!(r1.state, "exited");
        assert!(r1.notice.is_empty());
        assert_eq!(r1.token, "token-1");

        let r2 = resumed.result("coder-2").unwrap();
        assert_eq!(r2.state, "working");
        assert!(r2.notice.is_empty());
        assert_eq!(r2.token, "token-2");

        let r3 = resumed.result("reviewer-1").unwrap();
        assert_eq!(r3.state, "failed");
        assert!(r3.notice.is_empty());
        assert!(
            r3.error.contains("relánzalo"),
            "failed starting agent must explain how to relaunch: {}",
            r3.error
        );
        assert_eq!(r3.token, "token-3");
    }

    #[test]
    fn req5_kill_clean_focus_and_limits_confirmation() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());

        let brain = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "brain".into(),
                task: "lead".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();

        // Focus selects native pane/tab
        service.focus(&brain.id).unwrap();
        assert!(!terminals.focused().is_empty());

        // Mark brain as "done" -> kill must accept "done" and succeed even if pane is already gone
        let mut reg = service.load_registry().unwrap();
        reg.submit_result(&brain.id, "done planning", "2026-04-20T00:01:00Z")
            .unwrap();
        service.save_registry(&reg).unwrap();
        terminals.remove_status(&brain.id);

        let killed = service.kill(&brain.id).unwrap();
        assert_eq!(killed.state, "killed");

        // Killing an already-definitive agent must fail
        assert!(service.kill(&brain.id).is_err());

        // Spawn another agent, leave it in "working" -> clean must reject running agent
        let coder = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "coder".into(),
                task: "implement".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        assert!(service.clean(&[coder.id.clone()], None).is_err());

        // Clean path traversal validation
        assert!(service.clean(&["../evil".into()], None).is_err());
        assert!(service.clean(&["foo/bar".into()], None).is_err());

        // Mark coder as "done" -> clean by explicit ID kills it first and removes its session dir
        let mut reg = service.load_registry().unwrap();
        reg.submit_result(&coder.id, "implemented", "2026-04-20T00:02:00Z")
            .unwrap();
        service.save_registry(&reg).unwrap();
        let coder_session_dir = service.service_config().agent_session_dir(&coder.id);
        assert!(coder_session_dir.exists());

        let cleaned = service.clean(&[coder.id.clone()], None).unwrap();
        assert_eq!(cleaned, vec![coder.id.clone()]);
        assert!(!coder_session_dir.exists());
        assert!(terminals.closed().contains(&coder.tab));

        // Clean without explicit IDs removes all definitive agents (brain-1) while keeping counters
        let cleaned_all = service.clean(&[], None).unwrap();
        assert_eq!(cleaned_all, vec![brain.id.clone()]);
        let reg_after = service.load_registry().unwrap();
        assert!(reg_after.agents.is_empty());
        assert_eq!(reg_after.next.get("brain"), Some(&1));
        assert_eq!(reg_after.next.get("coder"), Some(&1));
    }

    #[test]
    fn req6_unique_agent_names_reclaims_shepherd_leftover_and_protects_user_panes() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());

        // 1. Pre-populate a Shepherd leftover pane for "coder-1" (`coder·codex·old-task`, not in registry)
        terminals.put_open_pane("coder-1", "pane-leftover", "tab-leftover", "coder·codex·old-task");

        let coder1 = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "coder".into(),
                task: "new task".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        assert_eq!(coder1.id, "coder-1");
        assert!(
            terminals.closed().contains(&"tab-leftover".to_owned()),
            "Shepherd leftover tab must be closed before retrying open_agent"
        );

        // 2. Pre-populate a user/non-Shepherd pane for "coder-2" (label "vim-notes")
        terminals.put_open_pane("coder-2", "pane-user", "tab-user", "vim-notes");
        let err = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "coder".into(),
                task: "another task".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap_err();
        assert!(
            err.contains("pane-user") && err.contains("vim-notes"),
            "error must explain which pane/label occupies the name: {err}"
        );
        assert!(
            !terminals.closed().contains(&"tab-user".to_owned()),
            "non-leftover pane must NEVER be closed"
        );

        // 3. Req 8: Verify failed request was logged in memory and on disk
        let failures = service.failed_requests();
        assert!(!failures.is_empty());
        assert_eq!(failures.last().unwrap().method, "spawn");
        let log_on_disk = std::fs::read_to_string(service.service_config().log_path()).unwrap();
        assert!(log_on_disk.contains("method=spawn"));
    }

    #[test]
    fn test_handle_call_tasks_runs_skills() {
        let tmp = TestDir::new("call-ops");
        let (service, _terminals, _clock) = test_service(tmp.path());

        // 1. Task create
        let create_args = serde_json::json!({
            "root": tmp.path().to_str().unwrap(),
            "title": "Test task F3b",
            "description": "Task description for testing",
            "criteria": ["- [ ] first criteria"],
            "attachments": []
        });
        let created = service.handle_call("task_create", create_args).unwrap();
        let task_id = created["status"]["id"].as_u64().unwrap();
        assert_eq!(task_id, 1);

        // 2. Tasks list
        let list_args = serde_json::json!({
            "root": tmp.path().to_str().unwrap()
        });
        let tasks = service.handle_call("tasks", list_args.clone()).unwrap();
        let arr = tasks.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["status"]["title"], "Test task F3b");

        // 3. Runs list
        let runs = service.handle_call("runs", list_args.clone()).unwrap();
        let runs_arr = runs.as_array().unwrap();
        assert_eq!(runs_arr.len(), 1);
        assert_eq!(runs_arr[0]["task_id"], 1);

        // 4. Skills list
        let skills = service.handle_call("skills", list_args).unwrap();
        assert!(skills.is_array());

        // 5. Task archive
        let archive_args = serde_json::json!({
            "root": tmp.path().to_str().unwrap(),
            "id": 1
        });
        let archived = service.handle_call("task_archive", archive_args).unwrap();
        assert_eq!(archived["archived"], true);

        // 6. Git status
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "master"])
            .current_dir(tmp.path())
            .output();
        let git_status = service
            .handle_call(
                "git_status",
                serde_json::json!({
                    "root": tmp.path().to_str().unwrap()
                }),
            )
            .unwrap();
        assert!(git_status["worktrees"].is_array());
    }

    #[test]
    fn format_exact_sender_and_content_truncation() {
        let mut registry = AgentRegistryFile::default();
        let a1 = registry.create(
            Agent {
                role: "coder".into(),
                ..Agent::default()
            },
            "token".into(),
            "2026-04-20T00:00:00Z",
        );
        assert_eq!(a1.id, "coder-1");
        assert_eq!(format_shep_sender("coder-1", &registry), "coder#1");
        assert_eq!(format_shep_sender("brain-20", &registry), "brain#20");
        assert_eq!(format_shep_sender("user", &registry), "user");

        let custom = Agent {
            id: "special_agent".into(),
            seq: 7,
            role: "reviewer".into(),
            ..Agent::default()
        };
        registry.agents.push(custom);
        assert_eq!(format_shep_sender("special_agent", &registry), "reviewer#7");

        let newlines = "first\nsecond\r\nthird\rfourth";
        assert_eq!(format_shep_content(newlines), "first second third fourth");

        let short_content = "short content";
        assert_eq!(format_shep_content(short_content), "short content");

        let long_content = "a".repeat(520);
        let truncated = format_shep_content(&long_content);
        assert_eq!(truncated.chars().count(), 501);
        assert!(truncated.ends_with('…'));
        assert_eq!(&truncated[..500], &"a".repeat(500));

        let formatted = format_shep_message("coder-1", "hello\nworld", &registry);
        assert_eq!(formatted, "[shep] from coder#1: hello world");
    }

    #[test]
    fn req7_deliver_bus_messages_order_once_non_mcp_and_persists_cursor() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());

        // Spawn non-MCP agent (docs) and MCP agent (coder)
        let non_mcp = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "docs".into(),
                task: "doc-task".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        assert!(!non_mcp.mcp);

        let mcp = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "coder".into(),
                task: "code-task".into(),
                mcp_url: Some("http://127.0.0.1:9999/mcp".into()),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        assert!(mcp.mcp);

        // Set non-MCP agent to idle and give it a screen
        terminals.set_status(
            &non_mcp.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "idle".into(),
                pane: non_mcp.pane.clone(),
                tab: non_mcp.tab.clone(),
                title: non_mcp.label.clone(),
                seq: 10,
            },
        );
        terminals.set_screen(&non_mcp.pane, "docs-shell$ ");

        // Set MCP agent to idle
        terminals.set_status(
            &mcp.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "idle".into(),
                pane: mcp.pane.clone(),
                tab: mcp.tab.clone(),
                title: mcp.label.clone(),
                seq: 10,
            },
        );
        terminals.set_screen(&mcp.pane, "mcp-shell$ ");

        // Send messages to the bus
        let bus = service.bus().unwrap();
        let _m1 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: non_mcp.id.clone(),
                kind: "msg".into(),
                content: "first instruction".into(),
            })
            .unwrap();
        let m2 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: non_mcp.id.clone(),
                kind: "msg".into(),
                content: "second instruction\nmultiline".into(),
            })
            .unwrap();
        let _m3 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: mcp.id.clone(),
                kind: "msg".into(),
                content: "for mcp agent".into(),
            })
            .unwrap();
        let _m4 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: "user".into(),
                kind: "msg".into(),
                content: "for user".into(),
            })
            .unwrap();

        // Run reconcile tick
        let reg = service.reconcile_once().unwrap();

        // 1. Check delivery to non-MCP agent: exact format, newlines as spaces, order
        let sent = terminals.sent_text();
        let non_mcp_sent: Vec<_> = sent
            .iter()
            .filter(|(target, _)| target == &non_mcp.pane || target == &non_mcp.id)
            .map(|(_, text)| text.as_str())
            .collect();

        assert_eq!(
            non_mcp_sent,
            vec![
                "[shep] from brain#1: first instruction",
                "[shep] from brain#1: second instruction multiline"
            ]
        );

        // 2. Neither MCP agent nor user received typed messages
        assert!(
            !sent.iter().any(|(t, _)| t == &mcp.pane || t == &mcp.id),
            "MCP agents must NOT receive typed messages in terminal"
        );
        assert!(
            !sent.iter().any(|(t, _)| t == "user"),
            "user messages must NOT be typed"
        );

        // 3. Cursor advanced to m2.id
        let updated_non_mcp = reg.result(&non_mcp.id).unwrap();
        assert_eq!(updated_non_mcp.delivery_cursor, m2.id);

        // 4. Second reconcile tick does not re-deliver
        let sent_len_before = terminals.sent_text().len();
        service.reconcile_once().unwrap();
        assert_eq!(
            terminals.sent_text().len(),
            sent_len_before,
            "messages must be delivered only once"
        );

        // 5. Restart service: cursor persists from disk, previously delivered messages are not resent
        let (service2, terminals2, _clock2) = test_service(tmp.path());
        terminals2.set_status(
            &non_mcp.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "idle".into(),
                pane: non_mcp.pane.clone(),
                tab: non_mcp.tab.clone(),
                title: non_mcp.label.clone(),
                seq: 11,
            },
        );
        terminals2.set_screen(&non_mcp.pane, "docs-shell$ ");

        service2.reconcile_once().unwrap();
        assert!(
            terminals2.sent_text().is_empty(),
            "cursor must persist across service restart"
        );

        // Send a new message after restart -> only the new message is delivered
        let m5 = service2
            .bus()
            .unwrap()
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "coder-1".into(),
                to: non_mcp.id.clone(),
                kind: "msg".into(),
                content: "third instruction".into(),
            })
            .unwrap();

        let reg2 = service2.reconcile_once().unwrap();
        let sent2 = terminals2.sent_text();
        assert_eq!(sent2.len(), 1);
        assert_eq!(sent2[0].1, "[shep] from coder#1: third instruction");
        assert_eq!(reg2.result(&non_mcp.id).unwrap().delivery_cursor, m5.id);
    }

    #[test]
    fn req7_deliver_bus_messages_blocked_working_and_terminal_states() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());

        let non_mcp = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "docs".into(),
                task: "doc-task".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        terminals.set_screen(&non_mcp.pane, "shell$ ");

        let bus = service.bus().unwrap();
        let m1 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: non_mcp.id.clone(),
                kind: "msg".into(),
                content: "do not type yet".into(),
            })
            .unwrap();

        // 1. Working: pane is busy -> no delivery
        terminals.set_status(
            &non_mcp.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "working".into(),
                pane: non_mcp.pane.clone(),
                tab: non_mcp.tab.clone(),
                title: non_mcp.label.clone(),
                seq: 1,
            },
        );
        let reg = service.reconcile_once().unwrap();
        assert!(terminals.sent_text().is_empty());
        assert_eq!(reg.result(&non_mcp.id).unwrap().delivery_cursor, 0);

        // 2. Blocked: trust dialog -> no delivery
        terminals.set_status(
            &non_mcp.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "blocked".into(),
                pane: non_mcp.pane.clone(),
                tab: non_mcp.tab.clone(),
                title: non_mcp.label.clone(),
                seq: 2,
            },
        );
        let reg = service.reconcile_once().unwrap();
        assert!(terminals.sent_text().is_empty());
        assert_eq!(reg.result(&non_mcp.id).unwrap().delivery_cursor, 0);

        // 3. Released to idle -> delivery succeeds!
        terminals.set_status(
            &non_mcp.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "idle".into(),
                pane: non_mcp.pane.clone(),
                tab: non_mcp.tab.clone(),
                title: non_mcp.label.clone(),
                seq: 3,
            },
        );
        let reg = service.reconcile_once().unwrap();
        assert_eq!(terminals.sent_text().len(), 1);
        assert_eq!(reg.result(&non_mcp.id).unwrap().delivery_cursor, m1.id);

        // 4. Agent transitions to "done" -> further messages are NOT delivered
        let mut reg_file = service.load_registry().unwrap();
        reg_file.agents[0].state = "done".into();
        service.save_registry(&reg_file).unwrap();

        let _m2 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: non_mcp.id.clone(),
                kind: "msg".into(),
                content: "agent is already done".into(),
            })
            .unwrap();

        let sent_before = terminals.sent_text().len();
        service.reconcile_once().unwrap();
        assert_eq!(
            terminals.sent_text().len(),
            sent_before,
            "done agents must NOT receive messages"
        );

        // 5. Agent transitions to "exited" (gone) -> no delivery
        let mut reg_file = service.load_registry().unwrap();
        reg_file.agents[0].state = "exited".into();
        service.save_registry(&reg_file).unwrap();

        let _m3 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: non_mcp.id.clone(),
                kind: "msg".into(),
                content: "agent is exited".into(),
            })
            .unwrap();

        service.reconcile_once().unwrap();
        assert_eq!(
            terminals.sent_text().len(),
            sent_before,
            "gone agents must NOT receive messages"
        );
    }

    #[test]
    fn req7_deliver_bus_messages_retry_on_verification_failure() {
        let tmp = TestDir::new("test");
        let (service, terminals, _clock) = test_service(tmp.path());

        let non_mcp = service
            .spawn(ServiceSpawnRequest {
                root: tmp.path().to_path_buf(),
                role: "docs".into(),
                task: "doc-task".into(),
                ..ServiceSpawnRequest::default()
            })
            .unwrap();
        terminals.set_status(
            &non_mcp.id,
            TerminalAgentStatus {
                state: "live".into(),
                status: "idle".into(),
                pane: non_mcp.pane.clone(),
                tab: non_mcp.tab.clone(),
                title: non_mcp.label.clone(),
                seq: 1,
            },
        );
        terminals.set_screen(&non_mcp.pane, "shell$ ");

        let bus = service.bus().unwrap();
        let m1 = bus
            .send(Message {
                id: 0,
                ts: String::new(),
                from: "brain-1".into(),
                to: non_mcp.id.clone(),
                kind: "msg".into(),
                content: "needs verification".into(),
            })
            .unwrap();

        // Disable echo on send_text so verification fails
        terminals.set_echo_on_send_text(false);

        // Reconcile tick runs, retries 3 times, records failure, does NOT fail tick
        let reg = service.reconcile_once().unwrap();
        assert_eq!(
            reg.result(&non_mcp.id).unwrap().delivery_cursor,
            0,
            "cursor must not advance if verification fails"
        );

        // Check service log has the failure recorded
        let log_content =
            std::fs::read_to_string(tmp.path().join("state").join("service.log")).unwrap_or_default();
        assert!(log_content.contains("was not verified on screen"));

        // Now enable echo on send_text and run next tick -> succeeds and advances cursor!
        terminals.set_echo_on_send_text(true);
        let reg = service.reconcile_once().unwrap();
        assert_eq!(
            reg.result(&non_mcp.id).unwrap().delivery_cursor,
            m1.id,
            "cursor must advance when verification succeeds"
        );
    }
}
