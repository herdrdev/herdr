//! In-process Shepherd runtime orchestration service for Shepherd (Phase F2).
//!
//! Runs outside the TUI render loop on background worker threads and communicates
//! with `App` exclusively through the `ShepherdInProcess` channel (`ApiRequestSender` + `EventHub`).

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Condvar, Mutex,
};
use std::thread::JoinHandle;

use serde_json::{json, Value};
use shepherd_core::{
    config as shep_config,
    mcp::RunningMcpServer,
    service::{Clock, OrchestrationService, ServiceConfig, SystemClock},
    terminals::Terminals,
    AgentRegistryFile,
};

use crate::api::{ApiRequestSender, EventHub};
use crate::shepherd::in_process_from_app_channel;

pub struct ShepherdCallJob {
    pub request_id: String,
    pub op: String,
    pub args: Value,
    pub respond_to: mpsc::Sender<String>,
}

/// Runtime handle for the in-process Shepherd orchestration service.
pub struct ShepherdRuntimeService {
    core: Arc<OrchestrationService>,
    latest_registry: Arc<Mutex<AgentRegistryFile>>,
    call_tx: Option<mpsc::Sender<ShepherdCallJob>>,
    stop_flag: Arc<(Mutex<bool>, Condvar)>,
    running: Arc<AtomicBool>,
    mcp_server: Option<RunningMcpServer>,
    call_worker: Option<JoinHandle<()>>,
    reconcile_worker: Option<JoinHandle<()>>,
    resume_done: Arc<AtomicBool>,
}

impl ShepherdRuntimeService {
    /// Start the Shepherd runtime service connected to `App` via `api_tx` and `event_hub`.
    /// Immediately performs startup `resume` (Requirement 4), starts the loopback HTTP MCP
    /// endpoint (Requirement 7), and launches background workers for `shepherd.call` and
    /// periodic reconciliation (Requirement 3).
    pub fn start_for_app(
        api_tx: ApiRequestSender,
        event_hub: EventHub,
        session: Option<&str>,
        state_dir: Option<PathBuf>,
        project_root: Option<PathBuf>,
    ) -> Result<Self, String> {
        let terminals: Arc<dyn Terminals> =
            Arc::new(in_process_from_app_channel(api_tx, event_hub, session));
        let svc_cfg = match state_dir {
            Some(dir) => ServiceConfig::for_state_dir(dir),
            None => ServiceConfig::default(),
        };
        let root = project_root
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let shep_cfg = shep_config::load_config(&root).unwrap_or_default();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
        Self::start_with_dependencies(svc_cfg, shep_cfg, terminals, clock, None, true)
    }

    /// Start the runtime service with explicit dependencies (used by deterministic tests).
    pub fn start_with_dependencies(
        svc_cfg: ServiceConfig,
        shep_cfg: shep_config::Config,
        terminals: Arc<dyn Terminals>,
        clock: Arc<dyn Clock>,
        active_vendors: Option<Vec<String>>,
        enable_reconcile_loop: bool,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&svc_cfg.state_dir).map_err(|e| e.to_string())?;

        let mcp_server = RunningMcpServer::start(
            0,
            svc_cfg.state_dir.clone(),
            Some(Arc::clone(&terminals)),
            active_vendors.clone(),
        )
        .map_err(|e| format!("failed to start in-process MCP server: {e}"))?;
        let mcp_url = mcp_server.url().to_owned();

        let mut core = OrchestrationService::new(svc_cfg, shep_cfg, terminals, clock);
        if let Some(vendors) = active_vendors {
            core = core.with_active_vendors(vendors);
        }
        core.set_mcp_url(Some(mcp_url));
        let core = Arc::new(core);

        // Load initial registry without blocking startup; resume runs deferred on the service thread.
        let initial_registry = core.load_registry().unwrap_or_default();
        let latest_registry = Arc::new(Mutex::new(initial_registry));

        let (call_tx, call_rx) = mpsc::channel::<ShepherdCallJob>();
        let stop_flag = Arc::new((Mutex::new(false), Condvar::new()));
        let running = Arc::new(AtomicBool::new(true));
        let resume_done = Arc::new(AtomicBool::new(false));

        let core_for_calls = Arc::clone(&core);
        let reg_for_calls = Arc::clone(&latest_registry);
        let resume_done_calls = Arc::clone(&resume_done);
        let call_worker = std::thread::Builder::new()
            .name("shepherd-call-worker".into())
            .spawn(move || {
                while let Ok(job) = call_rx.recv() {
                    if !resume_done_calls.swap(true, Ordering::SeqCst) {
                        if let Ok(reg) = core_for_calls.resume_on_startup() {
                            if let Ok(mut guard) = reg_for_calls.lock() {
                                *guard = reg;
                            }
                        }
                    }
                    let result = core_for_calls.handle_call(&job.op, job.args);
                    if let Ok(reg) = core_for_calls.load_registry() {
                        if let Ok(mut guard) = reg_for_calls.lock() {
                            *guard = reg;
                        }
                    }
                    let payload = match result {
                        Ok(data) => json!({
                            "id": job.request_id,
                            "result": {
                                "type": "shepherd_call",
                                "data": data,
                            }
                        }),
                        Err(message) => json!({
                            "id": job.request_id,
                            "error": {
                                "code": "shepherd_error",
                                "message": message,
                            }
                        }),
                    };
                    let _ = job.respond_to.send(payload.to_string());
                }
            })
            .map_err(|e| e.to_string())?;

        let reconcile_worker = if enable_reconcile_loop {
            let core_for_rec = Arc::clone(&core);
            let reg_for_rec = Arc::clone(&latest_registry);
            let stop_for_rec = Arc::clone(&stop_flag);
            let resume_done_rec = Arc::clone(&resume_done);
            // Backup polling interval increased; events wake up immediately.
            let backup_interval = std::time::Duration::from_secs(10);
            Some(
                std::thread::Builder::new()
                    .name("shepherd-reconcile-worker".into())
                    .spawn(move || {
                        let (lock, _cvar) = &*stop_for_rec;
                        loop {
                            if let Ok(guard) = lock.lock() {
                                if *guard {
                                    break;
                                }
                            }
                            // Event-driven wait: returns Ok(true) immediately on agent status change,
                            // or Ok(false) on backup timeout (10s)
                            let _ = core_for_rec.terminals().wait_agent_event(backup_interval);

                            if let Ok(guard) = lock.lock() {
                                if *guard {
                                    break;
                                }
                            }
                            if resume_done_rec.load(Ordering::SeqCst) {
                                if let Ok(reg) = core_for_rec.reconcile_once() {
                                    if let Ok(mut latest) = reg_for_rec.lock() {
                                        *latest = reg;
                                    }
                                }
                            }
                        }
                    })
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };

        Ok(Self {
            core,
            latest_registry,
            call_tx: Some(call_tx),
            stop_flag,
            running,
            mcp_server: Some(mcp_server),
            call_worker: Some(call_worker),
            reconcile_worker,
            resume_done,
        })
    }

    pub fn core(&self) -> &Arc<OrchestrationService> {
        &self.core
    }

    pub fn mcp_url(&self) -> Option<String> {
        self.core.mcp_url()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Return the latest cached `AgentRegistryFile` snapshot without performing disk I/O.
    pub fn latest_registry_snapshot(&self) -> AgentRegistryFile {
        if let Ok(reg) = self.core.load_registry() {
            if let Ok(mut guard) = self.latest_registry.lock() {
                *guard = reg.clone();
            }
            return reg;
        }
        self.latest_registry
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Return the cached in-memory `AgentRegistryFile` without any disk I/O (safe for render/tick).
    pub fn cached_registry(&self) -> AgentRegistryFile {
        self.latest_registry
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Ensure startup resume has completed on the service thread.
    pub fn ensure_resumed(&self) {
        if !self.resume_done.swap(true, Ordering::SeqCst) {
            if let Ok(reg) = self.core.resume_on_startup() {
                if let Ok(mut guard) = self.latest_registry.lock() {
                    *guard = reg;
                }
            }
        }
    }

    /// Enqueue an asynchronous `shepherd.call` request so the caller (`App` / `HeadlessServer`)
    /// never blocks waiting for the service worker.
    pub fn enqueue_call(&self, job: ShepherdCallJob) -> Result<(), String> {
        let Some(ref tx) = self.call_tx else {
            return Err("Shepherd runtime service is stopped".into());
        };
        tx.send(job)
            .map_err(|_| "Shepherd runtime service call queue is closed".to_owned())
    }

    /// Synchronously execute a service call (for tests or direct callers outside the App loop).
    pub fn call_sync(&self, op: &str, args: Value) -> Result<Value, String> {
        let res = self.core.handle_call(op, args);
        if let Ok(reg) = self.core.load_registry() {
            if let Ok(mut guard) = self.latest_registry.lock() {
                *guard = reg;
            }
        }
        res
    }

    /// Trigger one reconciliation pass and update the cached registry snapshot.
    pub fn reconcile_now(&self) -> Result<AgentRegistryFile, String> {
        let reg = self.core.reconcile_once()?;
        if let Ok(mut guard) = self.latest_registry.lock() {
            *guard = reg.clone();
        }
        Ok(reg)
    }

    /// Cleanly stop background workers and the loopback HTTP MCP server.
    pub fn shutdown(&mut self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        self.call_tx.take();
        {
            let (lock, cvar) = &*self.stop_flag;
            if let Ok(mut stopped) = lock.lock() {
                *stopped = true;
                cvar.notify_all();
            }
        }
        if let Some(mut mcp) = self.mcp_server.take() {
            mcp.shutdown();
        }
        if let Some(handle) = self.reconcile_worker.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.call_worker.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ShepherdRuntimeService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_core::{
        service::ManualClock,
        terminals::{InMemoryTerminals, TerminalAgentStatus},
        Agent,
    };
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    #[test]
    fn runtime_service_resumes_on_startup_serves_in_process_mcp_and_stops_cleanly() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp_dir = std::env::temp_dir().join(format!(
            "shepherd-shep-runtime-svc-{}-{nonce}",
            std::process::id()
        ));
        let state_dir = tmp_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Pre-seed registry with a `starting` agent that is already working in the runtime
        let mut reg = AgentRegistryFile::default();
        let mut seeded = reg.create(
            Agent {
                role: "brain".into(),
                vendor: "claude".into(),
                pane: "w1:p1".into(),
                tab: "w1:t1".into(),
                root: tmp_dir.display().to_string(),
                notice: "old notice".into(),
                mcp: true,
                ..Agent::default()
            },
            "secret-brain-token".into(),
            "2026-04-20T00:00:00Z",
        );
        seeded.pane = "w1:p1".into();
        seeded.tab = "w1:t1".into();
        reg.agents[0] = seeded.clone();
        shepherd_core::save_agent_registry(state_dir.join("agents.json"), &reg).unwrap();

        let terminals = InMemoryTerminals::new();
        terminals.set_status(
            "brain-1",
            TerminalAgentStatus {
                state: "live".into(),
                status: "working".into(),
                pane: "w1:p1".into(),
                tab: "w1:t1".into(),
                title: "brain·claude·1".into(),
                seq: 2,
            },
        );
        let clock = ManualClock::new();

        let mut svc = ShepherdRuntimeService::start_with_dependencies(
            ServiceConfig::for_state_dir(&state_dir),
            shep_config::Config::default(),
            Arc::new(terminals),
            Arc::new(clock),
            Some(vec!["claude".into(), "codex".into(), "gemini".into()]),
            false,
        )
        .unwrap();

        assert!(svc.is_running());
        // At startup before any call is processed, cached registry is served as-is from disk
        let cached = svc.cached_registry();
        let starting_brain = cached.result("brain-1").unwrap();
        assert_eq!(starting_brain.state, "starting");
        assert_eq!(starting_brain.token, "secret-brain-token");

        // Verify in-process loopback HTTP MCP server is live and authenticates bearer token
        let mcp_url = svc.mcp_url().unwrap();
        assert!(mcp_url.starts_with("http://127.0.0.1:"));
        let addr_str = mcp_url
            .strip_prefix("http://")
            .unwrap()
            .strip_suffix("/mcp")
            .unwrap();
        let body = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }))
        .unwrap();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr_str}\r\nAuthorization: Bearer secret-brain-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(addr_str).unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"));
        assert!(resp.contains("submit_result"));
        assert!(resp.contains("spawn_agent"));
        assert!(resp.contains("get_workspace_instructions"));

        // Verify async enqueue_call works without blocking caller
        let (tx, rx) = mpsc::channel();
        svc.enqueue_call(ShepherdCallJob {
            request_id: "req-1".into(),
            op: "agents".into(),
            args: json!({}),
            respond_to: tx,
        })
        .unwrap();
        let raw_reply = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let parsed: Value = serde_json::from_str(&raw_reply).unwrap();
        assert_eq!(parsed["id"], "req-1");
        assert_eq!(parsed["result"]["type"], "shepherd_call");
        assert_eq!(parsed["result"]["data"][0]["id"], "brain-1");

        // After the first attended request, deferred startup resume has run
        let cached_resumed = svc.cached_registry();
        let resumed_brain = cached_resumed.result("brain-1").unwrap();
        assert_eq!(resumed_brain.state, "working");
        assert!(resumed_brain.notice.is_empty());

        svc.shutdown();
        assert!(!svc.is_running());
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_start_for_app_does_not_block_on_startup_with_non_empty_registry() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp_dir = std::env::temp_dir().join(format!(
            "shepherd-shep-nonblocking-startup-{}-{nonce}",
            std::process::id()
        ));
        let state_dir = tmp_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Seed with a non-empty registry (blocked agent with notice)
        let mut reg = AgentRegistryFile::default();
        let mut seeded = reg.create(
            Agent {
                id: "coder-1".into(),
                role: "coder".into(),
                vendor: "gemini".into(),
                pane: "w1:p1".into(),
                tab: "w1:t1".into(),
                workspace: "w1".into(),
                state: "blocked".into(),
                notice: "trust dialog: open tab and accept".into(),
                ..Agent::default()
            },
            "secret-coder-token".into(),
            "2026-11-04T07:50:00Z",
        );
        seeded.pane = "w1:p1".into();
        seeded.tab = "w1:t1".into();
        seeded.state = "blocked".into();
        seeded.notice = "trust dialog: open tab and accept".into();
        reg.agents[0] = seeded;
        shepherd_core::save_agent_registry(state_dir.join("agents.json"), &reg).unwrap();

        // Create an api_tx whose receiver is NEVER read/drained (simulating App during boot)
        let (api_tx, _api_rx) = tokio::sync::mpsc::unbounded_channel();
        let event_hub = crate::shepherd::EventHub::default();

        let start_time = std::time::Instant::now();
        let mut svc = ShepherdRuntimeService::start_for_app(
            api_tx,
            event_hub,
            Some("iso"),
            Some(state_dir),
            Some(tmp_dir.clone()),
        )
        .expect("start_for_app must succeed immediately without blocking");

        let elapsed = start_time.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "start_for_app must never block on unserviced App channel, took {elapsed:?}"
        );

        // Cached registry must be served immediately as-is
        let cached = svc.cached_registry();
        assert_eq!(cached.agents.len(), 1);
        let coder = cached
            .result("coder-1")
            .expect("coder-1 in cached registry");
        assert_eq!(coder.state, "blocked");
        assert_eq!(coder.notice, "trust dialog: open tab and accept");

        svc.shutdown();
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
