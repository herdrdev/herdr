//! Stateless MCP Streamable HTTP endpoint for Shepherd agent tools.
//! The listener is loopback-only; each request is authenticated as an agent with its bearer token.

use crate::{
    agents_json,
    bus::{Message, MessageBus, KIND_MESSAGE, KIND_RESULT},
    config, find_task, load_agent_registry, read_task_section, save_agent_registry,
    skills::{self, SkillAdjust},
    spawn::{self, SpawnOptions, SpawnRequest},
    task_context,
    terminals::{connect_terminals, Terminals},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone)]
struct State {
    registry: PathBuf,
    bus_path: PathBuf,
    mcp_url: Option<String>,
    active_vendors: Option<Vec<String>>,
    terminals: Option<Arc<dyn Terminals>>,
}

impl State {
    #[cfg(test)]
    fn new(registry: PathBuf) -> Self {
        let bus_path = registry
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("messages.jsonl");
        Self {
            registry,
            bus_path,
            mcp_url: None,
            active_vendors: None,
            terminals: None,
        }
    }

    fn resolve_terminals(
        &self,
        root: &Path,
    ) -> Result<Box<dyn Terminals>, String> {
        if let Some(t) = &self.terminals {
            return Ok(Box::new(SharedTerminals(Arc::clone(t))));
        }
        let cfg = config::load_config(root).map_err(|e| e.to_string())?;
        connect_terminals(&cfg).map_err(|e| e.to_string())
    }
}

struct SharedTerminals(Arc<dyn Terminals>);

impl Terminals for SharedTerminals {
    fn open_agent(
        &self,
        request: &crate::terminals::OpenAgentRequest,
    ) -> Result<crate::terminals::OpenedAgent, crate::terminals::TerminalError> {
        self.0.open_agent(request)
    }
    fn prompt(
        &self,
        agent_or_pane: &str,
        text: &str,
    ) -> Result<crate::terminals::PromptOutcome, crate::terminals::TerminalError> {
        self.0.prompt(agent_or_pane, text)
    }
    fn send_text(
        &self,
        pane: &str,
        text: &str,
    ) -> Result<(), crate::terminals::TerminalError> {
        self.0.send_text(pane, text)
    }
    fn send_keys(
        &self,
        pane: &str,
        keys: &[String],
    ) -> Result<(), crate::terminals::TerminalError> {
        self.0.send_keys(pane, keys)
    }
    fn read_screen(
        &self,
        pane: &str,
        lines: u32,
    ) -> Result<String, crate::terminals::TerminalError> {
        self.0.read_screen(pane, lines)
    }
    fn wait(
        &self,
        agent_or_pane: &str,
        timeout: Duration,
    ) -> Result<crate::terminals::TerminalWaitResult, crate::terminals::TerminalError> {
        self.0.wait(agent_or_pane, timeout)
    }
    fn agent_status(
        &self,
        agent_or_pane: &str,
    ) -> Result<crate::terminals::TerminalAgentStatus, crate::terminals::TerminalError> {
        self.0.agent_status(agent_or_pane)
    }
    fn label(
        &self,
        pane: &str,
        labels: &std::collections::HashMap<String, String>,
    ) -> Result<(), crate::terminals::TerminalError> {
        self.0.label(pane, labels)
    }
    fn close(&self, tab_or_pane: &str) -> Result<(), crate::terminals::TerminalError> {
        self.0.close(tab_or_pane)
    }
    fn attach(
        &self,
        target: Option<&str>,
        takeover: bool,
    ) -> Result<(), crate::terminals::TerminalError> {
        self.0.attach(target, takeover)
    }
    fn focus(&self, target: &str) -> Result<(), crate::terminals::TerminalError> {
        self.0.focus(target)
    }
}

pub struct RunningMcpServer {
    addr: std::net::SocketAddr,
    url: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl RunningMcpServer {
    pub fn start(
        port: u16,
        state_dir: PathBuf,
        terminals: Option<Arc<dyn Terminals>>,
        active_vendors: Option<Vec<String>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/mcp");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let state = Arc::new(Mutex::new(State {
            registry: state_dir.join("agents.json"),
            bus_path: state_dir.join("messages.jsonl"),
            mcp_url: Some(url.clone()),
            active_vendors,
            terminals,
        }));
        let join = std::thread::Builder::new()
            .name("shepherd-mcp-http".into())
            .spawn(move || {
                for incoming in listener.incoming() {
                    if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = incoming {
                        let state = Arc::clone(&state);
                        std::thread::spawn(move || {
                            let _ = handle_http(stream, state);
                        });
                    } else {
                        break;
                    }
                }
            })?;
        Ok(Self {
            addr,
            url,
            stop,
            join: Some(join),
        })
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn shutdown(&mut self) {
        if !self.stop.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for RunningMcpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn serve(port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_with_terminals(port, None)
}

pub fn serve_with_terminals(
    port: u16,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let addr = listener.local_addr()?;
    println!("MCP endpoint: http://{addr}/mcp");
    serve_listener(listener, Some(format!("http://{addr}/mcp")), terminals)
}

fn serve_listener(
    listener: TcpListener,
    mcp_url: Option<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state_dir = config::state_dir();
    let state = Arc::new(Mutex::new(State {
        registry: state_dir.join("agents.json"),
        bus_path: state_dir.join("messages.jsonl"),
        mcp_url,
        active_vendors: None,
        terminals,
    }));
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    let _ = handle_http(stream, state);
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn handle_http(mut stream: TcpStream, state: Arc<Mutex<State>>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = Vec::with_capacity(4096);
    let mut buf = [0u8; 4096];
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= 16 * 1024 {
            return respond(&mut stream, 413, "text/plain", b"headers too large");
        }
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buf[..n]);
    };
    let header = std::str::from_utf8(&bytes[..header_end]).unwrap_or("");
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let mut auth = String::new();
    let mut content_length = None;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("authorization") {
                auth = value.trim().to_owned();
            }
            if key.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
    if method != "POST" || path != "/mcp" {
        return respond(&mut stream, 404, "text/plain", b"not found");
    }
    let Some(token) = auth.strip_prefix("Bearer ").map(str::to_owned) else {
        return respond(&mut stream, 401, "text/plain", b"unauthorized");
    };
    let length = content_length.unwrap_or(0);
    if length == 0 || length > MAX_BODY {
        return respond(&mut stream, 413, "text/plain", b"invalid request size");
    }
    while bytes.len() < header_end + length {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return respond(&mut stream, 400, "text/plain", b"incomplete body");
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    let request: Value = match serde_json::from_slice(&bytes[header_end..header_end + length]) {
        Ok(value) => value,
        Err(_) => return respond(&mut stream, 400, "text/plain", b"invalid json"),
    };
    let state_snapshot = state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let result = rpc(&state_snapshot, &token, &request);
    match result {
        RpcReply::Notification => respond(&mut stream, 202, "application/json", b""),
        RpcReply::Json(value) => {
            let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
            respond(&mut stream, 200, "application/json", &body)
        }
        RpcReply::Unauthorized => respond(&mut stream, 401, "text/plain", b"unauthorized"),
    }
}

enum RpcReply {
    Json(Value),
    Notification,
    Unauthorized,
}

fn rpc(state: &State, token: &str, request: &Value) -> RpcReply {
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return request
            .get("id")
            .map(|id| rpc_error(id.clone(), -32600, "invalid request"))
            .unwrap_or(RpcReply::Notification);
    };
    let agent = match load_agent_registry(&state.registry) {
        Ok(registry) => match registry.by_token(token) {
            Some(agent) => agent.clone(),
            None => return RpcReply::Unauthorized,
        },
        Err(_) => return RpcReply::Unauthorized,
    };
    let Some(id) = request.get("id") else {
        return RpcReply::Notification;
    };
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2025-03-26",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "shepherd", "version": "0.1.0" }
        }),
        "ping" => json!({}),
        "tools/list" => tool_list(),
        "tools/call" => match call_tool(state, &agent, request.get("params").unwrap_or(&Value::Null))
        {
            Ok(result) => result,
            Err(error) => {
                return RpcReply::Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": error}],
                        "isError": true
                    }
                }))
            }
        },
        _ => return rpc_error(id.clone(), -32601, "method not found"),
    };
    RpcReply::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn tool_list() -> Value {
    json!({"tools": [
        {
            "name": "spawn_agent",
            "description": "Start a sub-agent in its own terminal (requires a role with can_spawn: true).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role": {"type": "string"},
                    "task": {"type": "string"},
                    "vendor": {"type": "string"},
                    "model": {"type": "string"},
                    "task_id": {"type": "string"},
                    "use_worktree": {"type": "string"},
                    "add_skills": {"type": "array", "items": {"type": "string"}},
                    "remove_skills": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["role", "task"]
            }
        },
        {
            "name": "send_message",
            "description": "Send a message to another agent by ID or role.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["to", "content"]
            }
        },
        {
            "name": "read_messages",
            "description": "Read and clear your pending agent messages.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "wait_for",
            "description": "Block until every listed agent has submitted a result or exited.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent_ids": {"type": "array", "items": {"type": "string"}},
                    "timeout_seconds": {"type": "integer"}
                },
                "required": ["agent_ids"]
            }
        },
        {
            "name": "submit_result",
            "description": "Deliver your final result to Shepherd.",
            "inputSchema": {
                "type": "object",
                "properties": {"summary": {"type": "string"}},
                "required": ["summary"]
            }
        },
        {
            "name": "get_result",
            "description": "Return an agent's current state and submitted result without blocking.",
            "inputSchema": {
                "type": "object",
                "properties": {"agent_id": {"type": "string"}},
                "required": ["agent_id"]
            }
        },
        {
            "name": "list_agents",
            "description": "List all agents in this session with their role, vendor, state, and worktree.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "get_diff",
            "description": "Read a coder's complete worktree diff against the configured base branch.",
            "inputSchema": {
                "type": "object",
                "properties": {"agent_id": {"type": "string"}},
                "required": ["agent_id"]
            }
        },
        {
            "name": "read_output",
            "description": "Read the last N lines of an agent's terminal pane.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "lines": {"type": "integer"}
                },
                "required": ["agent_id"]
            }
        },
        {
            "name": "ask_docs",
            "description": "Send a question to the docs agent (spawning one if needed) and wait for its answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "timeout_seconds": {"type": "integer"}
                },
                "required": ["question"]
            }
        },
        {
            "name": "get_task_context",
            "description": "Read this task's issue, specification, acceptance criteria, links, or plan.",
            "inputSchema": {
                "type": "object",
                "properties": {"section": {"type": "string"}}
            }
        },
        {
            "name": "update_task_context",
            "description": "Update a task context section (brain and docs agents only).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "section": {"type": "string"},
                    "content": {"type": "string"},
                    "append": {"type": "boolean"}
                },
                "required": ["section", "content"]
            }
        },
        {
            "name": "get_workspace_instructions",
            "description": "Return workspace instructions, repo instruction files, effective skills, and detected checks.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ]})
}

fn call_tool(state: &State, agent: &crate::Agent, params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tool name is required")?;
    let args = params.get("arguments").unwrap_or(&Value::Null);
    let text = match name {
        "spawn_agent" => {
            let role = args
                .get("role")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("role is required")?;
            let task = args
                .get("task")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("task is required")?;
            let vendor = args
                .get("vendor")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let model = args
                .get("model")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let task_id = args
                .get("task_id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| (!agent.task_id.is_empty()).then(|| agent.task_id.clone()));
            let use_worktree = args
                .get("use_worktree")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let add_skills = string_array_arg(args, "add_skills");
            let remove_skills = string_array_arg(args, "remove_skills");

            let root = PathBuf::from(&agent.root);
            let terminals = state.resolve_terminals(&root)?;
            let spawned = spawn::spawn_child(
                SpawnRequest {
                    root,
                    role: role.to_owned(),
                    task: task.to_owned(),
                    task_id,
                    vendor,
                    model,
                    mcp_url: state.mcp_url.clone(),
                },
                SpawnOptions {
                    parent_id: Some(agent.id.clone()),
                    registry_path: Some(state.registry.clone()),
                    active_vendors: state.active_vendors.clone(),
                    terminals: terminals.as_ref(),
                },
                SkillAdjust {
                    add: add_skills,
                    remove: remove_skills,
                },
                use_worktree,
            )
            .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&spawned.agent.public()).map_err(|e| e.to_string())?
        }
        "submit_result" => {
            let summary = args
                .get("summary")
                .and_then(Value::as_str)
                .ok_or("summary is required")?;
            let mut registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            registry.submit_result(&agent.id, summary, &super::now_rfc3339())?;
            save_agent_registry(&state.registry, &registry).map_err(|error| error.to_string())?;
            if !agent.parent_id.is_empty() {
                if let Ok(bus) = MessageBus::open(&state.bus_path) {
                    let _ = bus.send(Message {
                        id: 0,
                        ts: String::new(),
                        from: agent.id.clone(),
                        to: agent.parent_id.clone(),
                        kind: KIND_RESULT.into(),
                        content: format!("{} finished: {}", agent.id, summary.trim()),
                    });
                }
            }
            "result recorded".to_owned()
        }
        "get_result" => {
            let id = args
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("agent_id is required")?;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let target = registry
                .result(id)
                .ok_or_else(|| format!("unknown agent {id:?}"))?;
            serde_json::to_string_pretty(&json!({
                "id": target.id,
                "state": target.state,
                "has_result": target.has_result,
                "result": target.result,
                "error": target.error,
                "worktree": target.worktree,
                "branch": target.branch
            }))
            .map_err(|e| e.to_string())?
        }
        "list_agents" => {
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            agents_json(&registry).map_err(|e| e.to_string())?
        }
        "wait_for" => {
            let ids = string_array_arg(args, "agent_ids");
            if ids.is_empty() {
                return Err("agent_ids is empty".into());
            }
            let timeout_secs = args
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(1800);
            let deadline = Instant::now() + Duration::from_secs(timeout_secs);
            loop {
                let registry =
                    load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
                let mut out = BTreeMap::new();
                let mut all_done = true;
                for id in &ids {
                    match registry.result(id) {
                        None => {
                            out.insert(id.clone(), json!({"error": "unknown agent"}));
                        }
                        Some(a) => {
                            if !a.has_result && !a.terminal() {
                                all_done = false;
                            }
                            out.insert(
                                id.clone(),
                                json!({
                                    "state": a.state,
                                    "result": a.result,
                                    "error": a.error
                                }),
                            );
                        }
                    }
                }
                if all_done {
                    break serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
                }
                if Instant::now() >= deadline {
                    let partial =
                        serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".into());
                    return Err(format!("timeout waiting for {ids:?}: {partial}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        "read_output" => {
            let id = args
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("agent_id is required")?;
            let lines = args
                .get("lines")
                .and_then(Value::as_u64)
                .filter(|&n| n > 0)
                .unwrap_or(80) as u32;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let target = registry
                .result(id)
                .ok_or_else(|| format!("unknown agent {id:?}"))?;
            let pane = if !target.pane.is_empty() {
                &target.pane
            } else {
                &target.id
            };
            let terminals = state.resolve_terminals(Path::new(&agent.root))?;
            terminals
                .read_screen(pane, lines)
                .map_err(|e| e.to_string())?
        }
        "ask_docs" => {
            let question = args
                .get("question")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("question is required")?;
            let timeout_secs = args
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(300);
            let root = PathBuf::from(&agent.root);
            let terminals = state.resolve_terminals(&root)?;

            let docs_id = {
                let registry =
                    load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
                if let Some(existing) = registry.live().into_iter().find(|a| a.role == "docs") {
                    existing.id.clone()
                } else {
                    let spawned = spawn::spawn_child(
                        SpawnRequest {
                            root: root.clone(),
                            role: "docs".into(),
                            task:
                                "Answer documentation questions from other agents via Shepherd messages."
                                    .into(),
                            task_id: (!agent.task_id.is_empty()).then(|| agent.task_id.clone()),
                            vendor: None,
                            model: None,
                            mcp_url: state.mcp_url.clone(),
                        },
                        SpawnOptions {
                            parent_id: None,
                            registry_path: Some(state.registry.clone()),
                            active_vendors: state.active_vendors.clone(),
                            terminals: terminals.as_ref(),
                        },
                        SkillAdjust::default(),
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                    spawned.agent.id
                }
            };

            let prompt_msg = format!(
                "Question from {}: {question}\nReply with send_message(to={:?}, content=...) or submit_result.",
                agent.id, agent.id
            );
            let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
            let sent = bus
                .send(Message {
                    id: 0,
                    ts: String::new(),
                    from: agent.id.clone(),
                    to: docs_id.clone(),
                    kind: KIND_MESSAGE.into(),
                    content: prompt_msg.clone(),
                })
                .map_err(|e| e.to_string())?;
            let _ = terminals.prompt(&docs_id, &prompt_msg);

            let deadline = Instant::now() + Duration::from_secs(timeout_secs);
            loop {
                let bus = MessageBus::open(&state.bus_path).map_err(|e| e.to_string())?;
                for msg in bus.since(sent.id).map_err(|e| e.to_string())? {
                    if msg.from == docs_id && msg.to == agent.id {
                        return Ok(tool_ok(msg.content));
                    }
                }
                let registry =
                    load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
                if let Some(docs_agent) = registry.result(&docs_id) {
                    if docs_agent.has_result {
                        return Ok(tool_ok(docs_agent.result.clone()));
                    }
                    if docs_agent.terminal() {
                        return Err(format!(
                            "docs agent {docs_id} ended in state {} without answering",
                            docs_agent.state
                        ));
                    }
                }
                if Instant::now() >= deadline {
                    return Err(format!("timed out waiting for docs agent {docs_id}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        "get_task_context" => {
            if agent.task_id.is_empty() {
                return Err("this agent has no task context".into());
            }
            let task =
                find_task(&agent.root, &agent.task_id).map_err(|error| error.to_string())?;
            match args
                .get("section")
                .and_then(Value::as_str)
                .filter(|section| !section.is_empty())
            {
                Some(section) => {
                    read_task_section(&task, section).map_err(|error| error.to_string())?
                }
                None => task_context(&task, 0),
            }
        }
        "read_messages" => {
            let bus = MessageBus::open(&state.bus_path).map_err(|error| error.to_string())?;
            let messages = bus.read(&agent.id).map_err(|error| error.to_string())?;
            serde_json::to_string(&json!({"messages": messages}))
                .map_err(|error| error.to_string())?
        }
        "send_message" => {
            let to = args
                .get("to")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or("recipient is required")?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or("message content is required")?;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let recipient = if to == "user" || registry.result(to).is_some() {
                to.to_owned()
            } else {
                registry
                    .live()
                    .into_iter()
                    .find(|candidate| candidate.role == to)
                    .map(|candidate| candidate.id.clone())
                    .ok_or_else(|| format!("no live agent or role named {to:?}"))?
            };
            let bus = MessageBus::open(&state.bus_path).map_err(|error| error.to_string())?;
            let message = bus
                .send(Message {
                    id: 0,
                    ts: String::new(),
                    from: agent.id.clone(),
                    to: recipient.clone(),
                    kind: KIND_MESSAGE.into(),
                    content: content.to_owned(),
                })
                .map_err(|error| error.to_string())?;
            format!("delivered message {} to {}", message.id, recipient)
        }
        "get_diff" => {
            let id = args
                .get("agent_id")
                .and_then(Value::as_str)
                .ok_or("agent_id is required")?;
            let registry =
                load_agent_registry(&state.registry).map_err(|error| error.to_string())?;
            let coder = registry
                .result(id)
                .ok_or_else(|| format!("unknown agent {id:?}"))?;
            if coder.worktree.is_empty() {
                return Err(format!("agent {id:?} has no worktree"));
            }
            let config = config::load_config(&coder.root).map_err(|error| error.to_string())?;
            let base = default_base(&coder.root, &config.workflow.base_branch);
            let diff = std::process::Command::new("git")
                .args(["diff", &base])
                .current_dir(&coder.worktree)
                .output()
                .map_err(|error| error.to_string())?;
            if !diff.status.success() {
                return Err(String::from_utf8_lossy(&diff.stderr).trim().to_owned());
            }
            let mut text = String::from_utf8_lossy(&diff.stdout).into_owned();
            let untracked = std::process::Command::new("git")
                .args(["ls-files", "--others", "--exclude-standard"])
                .current_dir(&coder.worktree)
                .output()
                .map_err(|error| error.to_string())?;
            if untracked.status.success() && !untracked.stdout.is_empty() {
                text.push_str("\n\n# untracked files (not in the diff above):\n# ");
                text.push_str(
                    &String::from_utf8_lossy(&untracked.stdout)
                        .trim()
                        .replace('\n', "\n# "),
                );
            }
            if text.len() > 200_000 {
                let mut end = 200_000;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str("\n... (diff truncated)");
            }
            text
        }
        "update_task_context" => {
            if !matches!(agent.role.as_str(), "brain" | "docs") {
                return Err(format!("role {:?} may not update task context", agent.role));
            }
            if agent.task_id.is_empty() {
                return Err("this agent has no task context".into());
            }
            let section = args
                .get("section")
                .and_then(Value::as_str)
                .ok_or("section is required")?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or("content is required")?;
            let task =
                find_task(&agent.root, &agent.task_id).map_err(|error| error.to_string())?;
            let content = if args
                .get("append")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let previous = read_task_section(&task, section).unwrap_or_default();
                format!("{}\n\n{}\n", previous.trim_end(), content)
            } else {
                content.to_owned()
            };
            crate::write_task_section(&task, section, &content)
                .map_err(|error| error.to_string())?;
            crate::touch_task(&task).map_err(|error| error.to_string())?;
            "task context updated".to_owned()
        }
        "get_workspace_instructions" => {
            let root = Path::new(&agent.root);
            let cfg = config::load_config(root).unwrap_or_default();
            let cat = skills::Catalog::load(Some(root), Some(&skills::user_skills_dir()));
            let (mut checks, langs) = skills::detect_checks(root);
            if !cfg.workflow.checks.is_empty() {
                checks = cfg.workflow.checks.clone();
            }
            let (eff, problems) = cat.effective_for(&cfg, &agent.role, &langs, &agent.skills);
            let skill_items: Vec<Value> = eff
                .into_iter()
                .map(|r| {
                    json!({
                        "name": r.skill.name,
                        "description": r.skill.description,
                        "scope": r.skill.scope,
                        "level": r.level,
                        "dir": r.skill.dir
                    })
                })
                .collect();
            let payload = json!({
                "instructions": skills::workspace_instructions(root),
                "repo_docs": skills::repo_instruction_docs(root),
                "skills": skill_items,
                "global_skills": cat.global_names(&cfg),
                "disabled": cfg.skills.disabled,
                "problems": problems,
                "checks": checks,
                "languages": langs
            });
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        }
        _ => return Err(format!("unknown tool {name:?}")),
    };
    Ok(tool_ok(text))
}

fn tool_ok(text: String) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": {"text": text},
        "isError": false
    })
}

fn string_array_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn default_base(root: &str, configured: &str) -> String {
    if !configured.is_empty() {
        return configured.to_owned();
    }
    if let Ok(output) = std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .current_dir(root)
        .output()
    {
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !value.is_empty() {
                return value;
            }
        }
    }
    for branch in ["main", "master", "develop"] {
        if std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", branch])
            .current_dir(root)
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return branch.into();
        }
    }
    "HEAD".into()
}

fn rpc_error(id: Value, code: i32, message: &str) -> RpcReply {
    RpcReply::Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn respond(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{save_agent_registry, terminals::InMemoryTerminals, Agent, AgentRegistryFile};

    #[test]
    fn authenticates_tokens_and_supports_mcp_initialize_and_tools() {
        let path = std::env::temp_dir().join(format!(
            "shepherd-mcp-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut registry = AgentRegistryFile::default();
        let agent = registry.create(
            Agent {
                role: "coder".into(),
                root: "/repo".into(),
                ..Agent::default()
            },
            "secret-token".into(),
            "now",
        );
        save_agent_registry(&path, &registry).unwrap();
        let state = State::new(path.clone());
        assert!(matches!(
            rpc(
                &state,
                "wrong",
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})
            ),
            RpcReply::Unauthorized
        ));
        let RpcReply::Json(initialized) = rpc(
            &state,
            "secret-token",
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        ) else {
            panic!("expected initialize response")
        };
        assert_eq!(initialized["result"]["serverInfo"]["name"], "shepherd");
        let RpcReply::Json(tools) = rpc(
            &state,
            "secret-token",
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        ) else {
            panic!("expected tools response")
        };
        let tool_names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(tool_names.len(), 13);
        for expected in [
            "spawn_agent",
            "send_message",
            "read_messages",
            "wait_for",
            "submit_result",
            "get_result",
            "list_agents",
            "get_diff",
            "read_output",
            "ask_docs",
            "get_task_context",
            "update_task_context",
            "get_workspace_instructions",
        ] {
            assert!(tool_names.contains(&expected), "missing MCP tool {expected}");
        }
        let RpcReply::Json(submitted) = rpc(
            &state,
            "secret-token",
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"submit_result","arguments":{"summary":"done"}}}),
        ) else {
            panic!("expected tool response")
        };
        assert_eq!(submitted["result"]["isError"], false);
        let saved = load_agent_registry(&path).unwrap();
        assert_eq!(saved.result(&agent.id).unwrap().result, "done");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn http_endpoint_accepts_authenticated_json_rpc() {
        use std::{io::BufReader, net::TcpListener};
        let path = std::env::temp_dir().join(format!(
            "shepherd-mcp-http-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut registry = AgentRegistryFile::default();
        registry.create(
            Agent {
                role: "coder".into(),
                root: "/repo".into(),
                ..Agent::default()
            },
            "http-token".into(),
            "now",
        );
        save_agent_registry(&path, &registry).unwrap();
        let state = Arc::new(Mutex::new(State::new(path.clone())));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            std::thread::spawn(move || handle_http(listener.accept().unwrap().0, state).unwrap());
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string();
        let mut client = TcpStream::connect(address).unwrap();
        write!(
            client,
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer http-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        let mut response = String::new();
        BufReader::new(client)
            .read_to_string(&mut response)
            .unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("submit_result"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn get_diff_reads_committed_coder_changes_against_configured_base() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("shepherd-mcp-diff-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(root.join(".shepherd")).unwrap();
        std::fs::write(
            root.join(".shepherd/config.yaml"),
            "workflow:\n  base_branch: main\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-b", "main"]).status.success());
        assert!(git(&["config", "user.email", "test@example.test"])
            .status
            .success());
        assert!(git(&["config", "user.name", "Shepherd Test"])
            .status
            .success());
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(git(&["add", "base.txt"]).status.success());
        assert!(git(&["commit", "-m", "base"]).status.success());
        assert!(git(&["checkout", "-b", "feature"]).status.success());
        std::fs::write(root.join("change.txt"), "review this change\n").unwrap();
        assert!(git(&["add", "change.txt"]).status.success());
        assert!(git(&["commit", "-m", "feature"]).status.success());

        let mut registry = AgentRegistryFile::default();
        registry.create(
            Agent {
                role: "coder".into(),
                root: root.display().to_string(),
                worktree: root.display().to_string(),
                branch: "feature".into(),
                ..Agent::default()
            },
            "coder-token".into(),
            "now",
        );
        let reviewer = registry.create(
            Agent {
                role: "reviewer".into(),
                root: root.display().to_string(),
                ..Agent::default()
            },
            "reviewer-token".into(),
            "now",
        );
        let registry_path = root.join("agents.json");
        save_agent_registry(&registry_path, &registry).unwrap();
        let state = State::new(registry_path);
        let result = call_tool(
            &state,
            &reviewer,
            &json!({"name":"get_diff","arguments":{"agent_id":"coder-1"}}),
        )
        .unwrap();
        assert!(result["structuredContent"]["text"]
            .as_str()
            .unwrap()
            .contains("review this change"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_brain_and_docs_can_update_task_plan() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("shepherd-mcp-plan-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let task = crate::create_task(
            &root,
            crate::NewTask {
                id: 2,
                title: "Plan".into(),
                description: String::new(),
                criteria: Vec::new(),
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();
        let mut registry = AgentRegistryFile::default();
        let brain = registry.create(
            Agent {
                role: "brain".into(),
                root: root.display().to_string(),
                task_id: "2".into(),
                ..Agent::default()
            },
            "brain-token".into(),
            "now",
        );
        let coder = registry.create(
            Agent {
                role: "coder".into(),
                root: root.display().to_string(),
                task_id: "2".into(),
                ..Agent::default()
            },
            "coder-token".into(),
            "now",
        );
        let registry_path = root.join("agents.json");
        save_agent_registry(&registry_path, &registry).unwrap();
        let state = State::new(registry_path);
        let result = call_tool(
            &state,
            &brain,
            &json!({"name":"update_task_context","arguments":{"section":"plan","content":"# Plan\n\n## Subtask 1: Work"}}),
        );
        assert!(result.is_ok());
        assert!(crate::read_task_section(&task, "plan")
            .unwrap()
            .contains("Subtask 1"));
        assert!(call_tool(
            &state,
            &coder,
            &json!({"name":"update_task_context","arguments":{"section":"plan","content":"bad"}}),
        )
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mcp_tools_spawn_wait_get_result_list_read_output_ask_docs_and_workspace_instructions() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "shepherd-mcp-r1-tools-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".shepherd")).unwrap();
        std::fs::write(root.join(".shepherd/workspace.md"), "Always run tests.\n").unwrap();
        std::fs::write(root.join("AGENTS.md"), "# Agent Rules\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        crate::create_task(
            &root,
            crate::NewTask {
                id: 1,
                title: "Feature X".into(),
                description: "Implement feature X".into(),
                criteria: vec!["Tests pass".into()],
                issue: None,
                issue_markdown: None,
                links: Vec::new(),
            },
        )
        .unwrap();

        let fake_terms = Arc::new(InMemoryTerminals::new());
        let registry_path = root.join("agents.json");
        let bus_path = root.join("messages.jsonl");
        let mut registry = AgentRegistryFile::default();
        let brain = registry.create(
            Agent {
                role: "brain".into(),
                vendor: "claude".into(),
                root: root.display().to_string(),
                task_id: "1".into(),
                depth: 0,
                ..Agent::default()
            },
            "brain-secret".into(),
            "now",
        );
        save_agent_registry(&registry_path, &registry).unwrap();

        let state = State {
            registry: registry_path.clone(),
            bus_path: bus_path.clone(),
            mcp_url: Some("http://127.0.0.1:4317/mcp".into()),
            active_vendors: Some(vec!["claude".into(), "codex".into(), "gemini".into()]),
            terminals: Some(fake_terms.clone()),
        };

        // 1. spawn_agent by brain (can_spawn: true) succeeds and redacts token
        let spawn_res = call_tool(
            &state,
            &brain,
            &json!({
                "name": "spawn_agent",
                "arguments": {
                    "role": "coder",
                    "task": "Implement feature X",
                    "add_skills": ["security-review"],
                    "remove_skills": ["conventional-commits"]
                }
            }),
        )
        .unwrap();
        let spawned_json: Value =
            serde_json::from_str(spawn_res["structuredContent"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(spawned_json["id"], "coder-1");
        assert_eq!(spawned_json["parent_id"], "brain-1");
        assert_eq!(spawned_json["depth"], 1);
        assert!(spawned_json.get("token").is_none());

        // 2. coder (can_spawn: false) cannot call spawn_agent
        let coder = load_agent_registry(&registry_path)
            .unwrap()
            .result("coder-1")
            .unwrap()
            .clone();
        let denied = call_tool(
            &state,
            &coder,
            &json!({
                "name": "spawn_agent",
                "arguments": {"role": "tester", "task": "Run tests"}
            }),
        );
        assert!(denied.unwrap_err().contains("can_spawn: false"));

        // 3. list_agents returns public list without tokens
        let list_res = call_tool(&state, &brain, &json!({"name": "list_agents"})).unwrap();
        let list_text = list_res["structuredContent"]["text"].as_str().unwrap();
        assert!(!list_text.contains("brain-secret"));
        assert!(!list_text.contains("\"token\""));
        let list_val: Value = serde_json::from_str(list_text).unwrap();
        assert_eq!(list_val.as_array().unwrap().len(), 2);

        // 4. read_output reads screen from Terminals adapter
        fake_terms.set_screen(&coder.pane, "line 1\nline 2\nline 3");
        let output_res = call_tool(
            &state,
            &brain,
            &json!({
                "name": "read_output",
                "arguments": {"agent_id": "coder-1", "lines": 2}
            }),
        )
        .unwrap();
        assert_eq!(
            output_res["structuredContent"]["text"].as_str().unwrap(),
            "line 2\nline 3"
        );

        // 5. submit_result from coder-1 + get_result + wait_for
        call_tool(
            &state,
            &coder,
            &json!({
                "name": "submit_result",
                "arguments": {"summary": "Feature X complete"}
            }),
        )
        .unwrap();

        let get_res = call_tool(
            &state,
            &brain,
            &json!({
                "name": "get_result",
                "arguments": {"agent_id": "coder-1"}
            }),
        )
        .unwrap();
        let get_val: Value =
            serde_json::from_str(get_res["structuredContent"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(get_val["state"], "done");
        assert_eq!(get_val["has_result"], true);
        assert_eq!(get_val["result"], "Feature X complete");

        let wait_res = call_tool(
            &state,
            &brain,
            &json!({
                "name": "wait_for",
                "arguments": {"agent_ids": ["coder-1"], "timeout_seconds": 2}
            }),
        )
        .unwrap();
        let wait_val: Value =
            serde_json::from_str(wait_res["structuredContent"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(wait_val["coder-1"]["state"], "done");
        assert_eq!(wait_val["coder-1"]["result"], "Feature X complete");

        // 6. ask_docs spawns docs-1 and receives its bus answer
        let bus_path_clone = bus_path.clone();
        let responder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            let bus = MessageBus::open(&bus_path_clone).unwrap();
            bus.send(Message {
                id: 0,
                ts: String::new(),
                from: "docs-1".into(),
                to: "brain-1".into(),
                kind: KIND_MESSAGE.into(),
                content: "See docs/rust-architecture.md for details.".into(),
            })
            .unwrap();
        });
        let ask_res = call_tool(
            &state,
            &brain,
            &json!({
                "name": "ask_docs",
                "arguments": {"question": "Where is the architecture doc?", "timeout_seconds": 5}
            }),
        )
        .unwrap();
        responder.join().unwrap();
        assert_eq!(
            ask_res["structuredContent"]["text"].as_str().unwrap(),
            "See docs/rust-architecture.md for details."
        );

        // 7. get_workspace_instructions returns workspace.md, repo_docs, skills, checks, languages
        let ws_res = call_tool(
            &state,
            &coder,
            &json!({"name": "get_workspace_instructions"}),
        )
        .unwrap();
        let ws_val: Value =
            serde_json::from_str(ws_res["structuredContent"]["text"].as_str().unwrap()).unwrap();
        assert_eq!(ws_val["instructions"], "Always run tests.");
        assert_eq!(ws_val["repo_docs"], json!(["AGENTS.md"]));
        assert_eq!(ws_val["checks"], json!(["cargo test"]));
        assert_eq!(ws_val["languages"], json!(["rust"]));
        let skill_names: Vec<&str> = ws_val["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert!(skill_names.contains(&"implement-task"));
        assert!(skill_names.contains(&"security-review"));
        assert!(!skill_names.contains(&"conventional-commits"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
