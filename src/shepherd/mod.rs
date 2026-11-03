//! Shepherd integration module for Herdr (Phase F1).
//!
//! Provides:
//! 1. In-process `Terminals` adapters (`HerdrInProcess`) backed either by:
//!    - `App`'s internal request channel (`ApiRequestSender` + `EventHub`) with zero socket I/O, or
//!    - Herdr's typed local `ApiClient` (`src/api/client.rs`) when `herdr shepherd ...` runs as a CLI process.
//! 2. `herdr shepherd <subcommand>` CLI delegation to `shepherd_core`.
//! 3. Read-only Shepherd agents TUI panel (`panel`).

// F1 in-process channel adapters and TUI panel state are exercised by F1 unit tests
// and wired into the runtime orchestration and interactive client loop in F2/F3.
#![allow(dead_code)]

pub mod panel;
pub mod runs;
pub mod service;
pub mod skills;
pub mod tasks;

pub use panel::ShepherdAgentsPanelState;
#[allow(unused_imports)]
pub use runs::ShepherdRunsState;
#[allow(unused_imports)]
pub use skills::ShepherdSkillsState;
#[allow(unused_imports)]
pub use tasks::ShepherdTasksState;
#[allow(unused_imports)] // Re-exported for server runtime and API dispatch in F2
pub use service::{ShepherdCallJob, ShepherdRuntimeService};

use serde_json::{json, Value};
use shepherd_core::{
    herdr::HerdrError,
    terminals::{BackendKind, HerdrInProcess, TerminalError, Terminals},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

use crate::api::{
    client::{parse_response_value, ApiClient, ApiClientError, ConnectionTarget},
    schema::{AgentTarget, EventData, EventKind, Method, Request},
    ApiRequestMessage, ApiRequestSender, EventHub,
};

const DEFAULT_IN_PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Route a single `ApiRequestMessage` onto an in-memory `App` using the same rules as `HeadlessServer`.
///
/// Specifically:
/// - `Method::AgentPrompt` is routed to `App::handle_deferred_agent_api_request`
/// - `Method::WorktreeCreate` is routed to `App::handle_deferred_worktree_api_request`
/// - All other `Method`s are routed to `App::handle_api_request_after_internal_events_drained`
pub(crate) fn dispatch_api_message_on_app(
    app: &mut crate::app::App,
    msg: ApiRequestMessage,
) {
    match &msg.request.method {
        Method::AgentPrompt(_) => {
            app.handle_deferred_agent_api_request(msg.request, msg.respond_to);
        }
        Method::WorktreeCreate(_) => {
            app.handle_deferred_worktree_api_request(msg.request, msg.respond_to, false);
        }
        _ => {
            let response = app.handle_api_request_after_internal_events_drained(msg.request);
            let _ = msg.respond_to.send(response);
        }
    }
}

/// Construct a `HerdrInProcess` adapter that talks directly to `App` over `ApiRequestSender`
/// and `EventHub` without opening any Unix socket.
pub fn in_process_from_app_channel(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
    session: Option<&str>,
) -> HerdrInProcess {
    let seq = AtomicU64::new(1);
    let session_name = session.unwrap_or("shepherd").to_owned();
    HerdrInProcess::new(session_name, move |method, params| {
        let id = format!("shepherd-inproc-{}", seq.fetch_add(1, Ordering::Relaxed));
        let request = decode_schema_request(&id, method, params.clone())?;
        match request.method {
            Method::AgentWait(wait_params) => {
                wait_for_agent_in_process(&id, wait_params, &api_tx, &event_hub)
            }
            _ => {
                let timeout = extract_timeout(&params).unwrap_or(DEFAULT_IN_PROCESS_TIMEOUT);
                dispatch_request_over_channel(request, &api_tx, timeout)
            }
        }
    })
}

/// Construct a `HerdrInProcess` adapter backed by Herdr's internal `ApiClient` (`src/api/client.rs`).
pub fn in_process_from_api_client(
    client: ApiClient,
    session: Option<&str>,
) -> HerdrInProcess {
    let seq = AtomicU64::new(1);
    let session_name = session.unwrap_or("shepherd").to_owned();
    HerdrInProcess::new(session_name, move |method, params| {
        let id = format!("shepherd-apiclient-{}", seq.fetch_add(1, Ordering::Relaxed));
        let timeout = extract_timeout(&params).unwrap_or(DEFAULT_IN_PROCESS_TIMEOUT);
        let request = decode_schema_request(&id, method, params)?;
        let raw = client
            .request_value_with_timeout(&request, timeout)
            .map_err(map_client_error)?;
        let response = parse_response_value(raw).map_err(map_client_error)?;
        serde_json::to_value(response.result).map_err(|e| HerdrError::Protocol(e.to_string()))
    })
}

/// Resolve the socket path for a Shepherd/Herdr session name and return a `HerdrInProcess`
/// adapter if a local Herdr server is reachable for that session.
pub fn try_connect_local_in_process(session: Option<&str>) -> Option<HerdrInProcess> {
    let socket_path = resolve_session_socket_path(session);
    let client = ApiClient::for_target(ConnectionTarget::SocketPath(socket_path));
    client
        .status_with_timeout(Duration::from_millis(500))
        .ok()?;
    Some(in_process_from_api_client(client, session))
}

fn resolve_session_socket_path(session: Option<&str>) -> PathBuf {
    if crate::session::explicit_session_requested() {
        return crate::session::active_api_socket_path();
    }
    match session.map(str::trim) {
        None | Some("") | Some("-") | Some(crate::session::DEFAULT_SESSION_NAME) => {
            crate::session::active_api_socket_path()
        }
        Some(named) => {
            if let Ok(explicit_socket) = std::env::var(crate::api::SOCKET_PATH_ENV_VAR) {
                if !explicit_socket.trim().is_empty() && crate::session::active_name().is_none() {
                    let named_path = crate::session::api_socket_path_for(Some(named));
                    if named_path.exists() {
                        return named_path;
                    }
                    return PathBuf::from(explicit_socket);
                }
            }
            crate::session::api_socket_path_for(Some(named))
        }
    }
}

/// Entrypoint for `herdr shepherd <args...>`.
pub fn run_cli(args: &[String]) -> std::io::Result<i32> {
    register_default_in_process_factory();

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = shepherd_core::cli::project_root(cwd);
    let config = shepherd_core::config::load_config(&root).unwrap_or_default();

    let terminals: Option<Arc<dyn Terminals>> = match shepherd_core::terminals::select_backend(&config)
    {
        Ok(BackendKind::Hc) => None,
        Ok(BackendKind::InProcess | BackendKind::Socket) | Err(_) => {
            let active_session = crate::session::active_name();
            let session_name = active_session
                .as_deref()
                .or(Some(config.herdr.session.as_str()));
            try_connect_local_in_process(session_name)
                .map(|adapter| Arc::new(adapter) as Arc<dyn Terminals>)
        }
    };

    match shepherd_core::run_cli_with_terminals(args.iter().skip(1).cloned(), terminals) {
        Ok(()) => Ok(0),
        Err(error) => {
            eprintln!("shepherd: {error}");
            Ok(1)
        }
    }
}

fn register_default_in_process_factory() {
    shepherd_core::terminals::set_in_process_terminals_factory(Some(Arc::new(|config| {
        let active_session = crate::session::active_name();
        let session_name = active_session
            .as_deref()
            .or(Some(config.herdr.session.as_str()));
        if let Some(adapter) = try_connect_local_in_process(session_name) {
            return Ok(Box::new(adapter));
        }
        Err(TerminalError::Protocol(format!(
            "no running local Herdr server found for session {:?}",
            session_name.unwrap_or("default")
        )))
    })));
}

fn decode_schema_request(
    id: &str,
    method: &str,
    params: Value,
) -> Result<Request, HerdrError> {
    let envelope = json!({
        "id": id,
        "method": method,
        "params": params,
    });
    serde_json::from_value(envelope).map_err(|err| {
        HerdrError::Protocol(format!("invalid Herdr API request for {method}: {err}"))
    })
}

fn extract_timeout(params: &Value) -> Option<Duration> {
    let ms = params.get("timeout_ms").and_then(Value::as_u64)?;
    Some(Duration::from_millis(ms).saturating_add(Duration::from_secs(5)))
}

fn dispatch_request_over_channel(
    request: Request,
    api_tx: &ApiRequestSender,
    timeout: Duration,
) -> Result<Value, HerdrError> {
    let request_id = request.id.clone();
    let (respond_tx, respond_rx) = mpsc::channel();
    api_tx
        .send(ApiRequestMessage {
            request,
            respond_to: respond_tx,
            response_write_complete: None,
        })
        .map_err(|_| HerdrError::Protocol("Herdr app channel is closed".into()))?;

    let raw = respond_rx.recv_timeout(timeout).map_err(|err| match err {
        mpsc::RecvTimeoutError::Timeout => HerdrError::Api {
            code: "timeout".into(),
            message: "timed out waiting for in-process Herdr app response".into(),
        },
        mpsc::RecvTimeoutError::Disconnected => {
            HerdrError::Protocol("Herdr app dropped response channel".into())
        }
    })?;

    parse_raw_api_response(&request_id, &raw)
}

fn parse_raw_api_response(expected_id: &str, raw: &str) -> Result<Value, HerdrError> {
    let value: Value =
        serde_json::from_str(raw).map_err(|e| HerdrError::Protocol(e.to_string()))?;
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        if id != expected_id {
            return Err(HerdrError::Protocol(format!(
                "response id {id:?} does not match request {expected_id:?}"
            )));
        }
    }
    if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("api_error")
            .to_owned();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Herdr API error")
            .to_owned();
        return Err(HerdrError::Api { code, message });
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| HerdrError::Protocol("success response has no result".into()))
}

fn wait_for_agent_in_process(
    request_id: &str,
    params: crate::api::schema::AgentWaitParams,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
) -> Result<Value, HerdrError> {
    if params.until.is_empty() {
        return Err(HerdrError::Api {
            code: "invalid_request".into(),
            message: "agent.wait requires a non-empty `until` status list".into(),
        });
    }
    let deadline = params
        .timeout_ms
        .map(|ms| Instant::now() + Duration::from_millis(ms));
    let mut last_sequence = event_hub.current_sequence();

    let get_agent = |suffix: &str| -> Result<crate::api::schema::AgentInfo, HerdrError> {
        let req = Request {
            id: format!("{request_id}:{suffix}"),
            method: Method::AgentGet(AgentTarget {
                target: params.target.clone(),
            }),
        };
        let result = dispatch_request_over_channel(req, api_tx, DEFAULT_IN_PROCESS_TIMEOUT)?;
        let agent_val = result.get("agent").cloned().unwrap_or(result);
        serde_json::from_value(agent_val)
            .map_err(|e| HerdrError::Protocol(format!("failed to decode agent_info: {e}")))
    };

    let current = get_agent("get")?;
    if params.until.contains(&current.agent_status) {
        return Ok(json!({
            "type": "agent_info",
            "agent": current,
        }));
    }
    let pane_id = current.pane_id.clone();

    loop {
        for (seq, envelope) in event_hub.events_after(last_sequence) {
            last_sequence = seq;
            if envelope.event == EventKind::PaneAgentStatusChanged {
                if let EventData::PaneAgentStatusChanged {
                    pane_id: ref event_pane_id,
                    ..
                } = envelope.data
                {
                    if event_pane_id == &pane_id {
                        let refreshed = get_agent("refresh")?;
                        if params.until.contains(&refreshed.agent_status) {
                            return Ok(json!({
                                "type": "agent_info",
                                "agent": refreshed,
                            }));
                        }
                    }
                }
            }
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                return Err(HerdrError::Api {
                    code: "timeout".into(),
                    message: format!("timed out waiting for agent {}", params.target),
                });
            }
        }
        std::thread::sleep(WAIT_POLL_INTERVAL);
    }
}

fn map_client_error(error: ApiClientError) -> HerdrError {
    match error {
        ApiClientError::Io(io) => HerdrError::Io(io),
        ApiClientError::Json(err) => HerdrError::Protocol(err.to_string()),
        ApiClientError::ErrorResponse(resp) => HerdrError::Api {
            code: resp.error.code,
            message: resp.error.message,
        },
        ApiClientError::EmptyResponse => HerdrError::Protocol("empty api response".into()),
        ApiClientError::UnexpectedResult(msg) => HerdrError::Protocol(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::panel::render_shepherd_agents_from_app;
    use super::*;
    use crate::api::schema::{AgentStatus, EventData, EventEnvelope, EventKind};
    use crate::app::{App, AppPolicy, AppState};
    use crate::config::Config;
    use ratatui::{buffer::Buffer, layout::Rect};
    use std::collections::HashMap;
    use tokio::sync::mpsc as tokio_mpsc;

    #[test]
    fn in_process_from_app_channel_drives_herdr_app_without_sockets() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _rt_guard = rt.enter();
        let event_hub = EventHub::default();
        let (api_tx, mut api_rx) = tokio_mpsc::unbounded_channel();
        let (_dummy_tx, dummy_rx) = tokio_mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            AppPolicy::TEST,
            None,
            dummy_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("shepherd-f1")];
        app.state.ensure_test_terminals();
        let root_pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(
            80,
            24,
            b"ready for shepherd f1\n",
        );
        app.terminal_runtimes.insert(terminal_id, runtime);

        let worker = std::thread::spawn(move || {
            while let Some(msg) = api_rx.blocking_recv() {
                dispatch_api_message_on_app(&mut app, msg);
            }
        });

        let terminals = in_process_from_app_channel(api_tx.clone(), event_hub, Some("shepherd"));
        assert!(terminals.client().is_in_process());

        // 1. Query session.snapshot in-process (no socket)
        let snapshot = terminals.client().snapshot().unwrap();
        let workspaces = snapshot["workspaces"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        let pane_id = snapshot["panes"][0]["pane_id"]
            .as_str()
            .unwrap()
            .to_owned();

        // 2. Rename and label pane via Terminals::label in-process
        terminals
            .label(
                &pane_id,
                &HashMap::from([
                    ("role".into(), "coder".into()),
                    ("agent".into(), "coder-1".into()),
                    ("vendor".into(), "codex".into()),
                    ("title".into(), "coder·codex·f1".into()),
                ]),
            )
            .unwrap();

        let updated_snapshot = terminals.client().snapshot().unwrap();
        let tokens = &updated_snapshot["panes"][0]["tokens"];
        assert_eq!(tokens["role"], "coder");
        assert_eq!(tokens["agent"], "coder-1");
        assert_eq!(tokens["vendor"], "codex");

        // 3. Read screen output via Terminals::read_screen in-process
        let screen = terminals.read_screen(&pane_id, 20).unwrap();
        assert!(screen.contains("ready for shepherd f1"));

        drop(terminals);
        drop(api_tx);
        worker.join().unwrap();
    }

    #[test]
    fn in_process_agent_wait_resolves_from_event_hub_without_socket() {
        let event_hub = EventHub::default();
        let (api_tx, mut api_rx) = tokio_mpsc::unbounded_channel::<ApiRequestMessage>();
        let worker_hub = event_hub.clone();

        let worker = std::thread::spawn(move || {
            let mut calls = 0usize;
            while let Some(msg) = api_rx.blocking_recv() {
                calls += 1;
                let status = if calls == 1 {
                    AgentStatus::Working
                } else {
                    AgentStatus::Idle
                };
                let reply = serde_json::json!({
                    "id": msg.request.id,
                    "result": {
                        "type": "agent_info",
                        "agent": {
                            "terminal_id": "t1",
                            "name": "coder-1",
                            "agent": "codex",
                            "agent_status": status,
                            "workspace_id": "w1",
                            "tab_id": "w1:t1",
                            "pane_id": "w1:p1",
                            "focused": true,
                            "state_change_seq": calls as u64,
                            "revision": calls as u64,
                            "tokens": {}
                        }
                    }
                });
                let _ = msg.respond_to.send(reply.to_string());
                if calls == 1 {
                    worker_hub.push(EventEnvelope {
                        event: EventKind::PaneAgentStatusChanged,
                        data: EventData::PaneAgentStatusChanged {
                            workspace_id: "w1".into(),
                            pane_id: "w1:p1".into(),
                            agent: Some("codex".into()),
                            agent_status: AgentStatus::Idle,
                            title: None,
                            display_agent: None,
                            state_labels: HashMap::new(),
                        },
                    });
                }
            }
        });

        let terminals = in_process_from_app_channel(api_tx.clone(), event_hub, Some("shepherd"));
        let waited = terminals.wait("coder-1", Duration::from_secs(2)).unwrap();
        assert!(waited.ended);
        assert_eq!(waited.status.as_deref(), Some("idle"));

        drop(terminals);
        drop(api_tx);
        worker.join().unwrap();
    }

    #[test]
    fn herdr_shepherd_cli_subcommands_run_agents_tasks_and_roles_json() {
        assert!(matches!(
            crate::cli::maybe_run(&[
                "herdr".into(),
                "shepherd".into(),
                "agents".into(),
                "--json".into()
            ])
            .unwrap(),
            crate::cli::CommandOutcome::Handled(0)
        ));
        assert_eq!(
            run_cli(&["shepherd".into(), "agents".into(), "--json".into()]).unwrap(),
            0
        );
        assert_eq!(
            run_cli(&[
                "shepherd".into(),
                "task".into(),
                "list".into(),
                "--json".into()
            ])
            .unwrap(),
            0
        );
        assert_eq!(
            run_cli(&[
                "shepherd".into(),
                "roles".into(),
                "list".into(),
                "--json".into()
            ])
            .unwrap(),
            0
        );
    }

    #[test]
    fn shepherd_agents_panel_renders_pure_state_and_highlights_needs_you() {
        let mut registry = shepherd_core::AgentRegistryFile::default();
        let mut brain = registry.create(
            shepherd_core::Agent {
                role: "brain".into(),
                vendor: "claude".into(),
                model: "opus".into(),
                pane: "w1:p1".into(),
                task_text: "Plan F1".into(),
                ..shepherd_core::Agent::default()
            },
            "tok1".into(),
            "2026-11-03T16:00:00Z",
        );
        brain.state = "working".into();
        registry.agents[0] = brain;

        let mut coder = registry.create(
            shepherd_core::Agent {
                role: "coder".into(),
                vendor: "codex".into(),
                model: "gpt-5".into(),
                pane: "w1:p2".into(),
                notice: "Waiting for tool permission in pane w1:p2".into(),
                ..shepherd_core::Agent::default()
            },
            "tok2".into(),
            "2026-11-03T16:01:00Z",
        );
        coder.state = "blocked".into();
        registry.agents[1] = coder;

        let mut app = AppState::test_new();
        // When panel is closed, render_shepherd_agents_from_app returns None
        let area = Rect::new(0, 0, 100, 28);
        let mut buffer = Buffer::empty(area);
        assert!(render_shepherd_agents_from_app(&mut buffer, area, &app).is_none());

        // Populate pure state and open panel
        app.shepherd_agents_panel = ShepherdAgentsPanelState::from_registry(&registry);
        app.shepherd_agents_panel.refresh_from_registry(&registry);
        app.shepherd_agents_panel.open = true;
        assert_eq!(app.shepherd_agents_panel.needs_you_count(), 1);

        let rendered = render_shepherd_agents_from_app(&mut buffer, area, &app)
            .expect("panel should render when open");
        assert_eq!(rendered.rows.len(), 3); // brain-1 row + coder-1 row + coder-1 notice row

        let text = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Shepherd Agents"));
        assert!(text.contains("brain-1"));
        assert!(text.contains("role:brain"));
        assert!(text.contains("vendor:claude/opus"));
        assert!(text.contains("coder-1"));
        assert!(text.contains("role:coder"));
        assert!(text.contains("vendor:codex/gpt-5"));
        assert!(text.contains("state:blocked"));
        assert!(text.contains("NEEDS YOU"));
        assert!(text.contains("Waiting for tool permission in pane w1:p2"));
    }

    #[test]
    fn app_shepherd_enabled_config_hook_starts_service_on_boot_and_stops_on_shutdown() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _rt_guard = rt.enter();

        // 1. Default config has shepherd.enabled == false
        let default_cfg = Config::default();
        assert!(!default_cfg.shepherd.enabled);
        let event_hub = EventHub::default();
        let (api_tx, _api_rx) = tokio_mpsc::unbounded_channel();
        let (_dummy_tx, dummy_rx) = tokio_mpsc::unbounded_channel();
        let mut app_disabled = App::new(
            &default_cfg,
            AppPolicy::TEST,
            None,
            dummy_rx,
            event_hub.clone(),
        );
        app_disabled.start_shepherd_service_if_enabled(Some(api_tx.clone()));
        assert!(app_disabled.shepherd_service.is_none());

        // 2. When shepherd.enabled == true, service starts, resumes, and stops cleanly
        let mut enabled_cfg = Config::default();
        enabled_cfg.shepherd.enabled = true;
        let (_dummy_tx2, dummy_rx2) = tokio_mpsc::unbounded_channel();
        let mut app_enabled = App::new(
            &enabled_cfg,
            AppPolicy::TEST,
            None,
            dummy_rx2,
            event_hub,
        );
        app_enabled.start_shepherd_service_if_enabled(Some(api_tx));
        assert!(app_enabled.shepherd_service.is_some());
        assert!(app_enabled.shepherd_service.as_ref().unwrap().is_running());

        app_enabled.stop_shepherd_service();
        assert!(app_enabled.shepherd_service.is_none());
    }

    #[test]
    fn api_shepherd_call_routes_to_running_service_and_fails_cleanly_when_disabled() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _rt_guard = rt.enter();

        let event_hub = EventHub::default();
        let (_dummy_tx, dummy_rx) = tokio_mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            AppPolicy::TEST,
            None,
            dummy_rx,
            event_hub.clone(),
        );

        // 1. Without shepherd.enabled, shepherd.call returns shepherd_not_running error
        let req_disabled = Request {
            id: "call-1".into(),
            method: Method::ShepherdCall(crate::api::schema::shepherd::ShepherdCallParams {
                op: "agents".into(),
                args: json!({}),
            }),
        };
        let resp_raw = app.handle_api_request_after_internal_events_drained(req_disabled);
        let resp_json: Value = serde_json::from_str(&resp_raw).unwrap();
        assert_eq!(resp_json["error"]["code"], "shepherd_not_running");

        // 2. Start service on app
        let (api_tx, _api_rx) = tokio_mpsc::unbounded_channel();
        app.shepherd_enabled = true;
        app.start_shepherd_service_if_enabled(Some(api_tx));
        assert!(app.shepherd_service.is_some());

        // 3. With shepherd.enabled, shepherd.call { op: "agents" } returns shepherd_call result
        let req_enabled = Request {
            id: "call-2".into(),
            method: Method::ShepherdCall(crate::api::schema::shepherd::ShepherdCallParams {
                op: "agents".into(),
                args: json!({}),
            }),
        };
        let resp_raw2 = app.handle_api_request_after_internal_events_drained(req_enabled);
        let resp_json2: Value = serde_json::from_str(&resp_raw2).unwrap();
        assert_eq!(resp_json2["id"], "call-2");
        assert_eq!(resp_json2["result"]["type"], "shepherd_call");
        assert!(resp_json2["result"]["data"].is_array());

        app.stop_shepherd_service();
    }
}
