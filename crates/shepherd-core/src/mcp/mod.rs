//! Stateless MCP Streamable HTTP endpoint for Shepherd agent tools.
//! The listener is loopback-only; each request is authenticated as an agent with its bearer token.

pub mod events;
pub mod server;
pub mod tools;

pub use server::{serve, serve_with_terminals, RunningMcpServer};
pub(crate) use tools::default_base;

use crate::{
    config,
    terminals::{connect_terminals, Terminals},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct State {
    pub(crate) registry: PathBuf,
    pub(crate) bus_path: PathBuf,
    pub(crate) mcp_url: Option<String>,
    pub(crate) active_vendors: Option<Vec<String>>,
    pub(crate) terminals: Option<Arc<dyn Terminals>>,
}

impl State {
    #[cfg(test)]
    pub(crate) fn new(registry: PathBuf) -> Self {
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

    pub(crate) fn resolve_terminals(&self, root: &Path) -> Result<Box<dyn Terminals>, String> {
        if let Some(t) = &self.terminals {
            return Ok(Box::new(SharedTerminals(Arc::clone(t))));
        }
        let cfg = config::load_config(root).map_err(|e| e.to_string())?;
        connect_terminals(&cfg).map_err(|e| e.to_string())
    }
}

pub(crate) struct SharedTerminals(pub(crate) Arc<dyn Terminals>);

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
        wait: Option<crate::shepherd::PromptWaitOptions>,
    ) -> Result<crate::terminals::PromptOutcome, crate::terminals::TerminalError> {
        self.0.prompt(agent_or_pane, text, wait)
    }
    fn send_text(&self, pane: &str, text: &str) -> Result<(), crate::terminals::TerminalError> {
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

#[cfg(test)]
mod tests {
    use super::server::{handle_http, rpc, RpcReply};
    use super::tools::call_tool;
    use super::*;
    use crate::{
        bus::{Message, MessageBus, KIND_MESSAGE},
        load_agent_registry, save_agent_registry,
        terminals::InMemoryTerminals,
        Agent, AgentRegistryFile,
    };
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex;

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
        assert_eq!(tool_names.len(), 19);
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
            "publish",
            "subscribe",
            "poll",
            "ack",
            "replay",
            "list_topics",
        ] {
            assert!(
                tool_names.contains(&expected),
                "missing MCP tool {expected}"
            );
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
            bus.send(Message::directed(
                "docs-1",
                "brain-1",
                KIND_MESSAGE,
                "See docs/rust-architecture.md for details.",
            ))
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

    #[test]
    fn mcp_event_tools_publish_subscribe_poll_ack_replay_and_topics() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "shepherd-mcp-events-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let registry_path = root.join("agents.json");

        let brain = Agent {
            id: "brain-1".into(),
            token: "brain-token".into(),
            role: "brain".into(),
            state: "working".into(),
            root: root.display().to_string(),
            ..Agent::default()
        };
        let coder = Agent {
            id: "coder-1".into(),
            token: "coder-token".into(),
            role: "coder".into(),
            state: "working".into(),
            root: root.display().to_string(),
            ..Agent::default()
        };

        save_agent_registry(
            &registry_path,
            &AgentRegistryFile {
                seq: 2,
                next: std::collections::HashMap::new(),
                agents: vec![brain.clone(), coder.clone()],
            },
        )
        .unwrap();

        let state = State::new(registry_path.clone());

        // 1. brain-1 publishes to topic "orders"
        let pub_reply = rpc(
            &state,
            "brain-token",
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "publish",
                    "arguments": {
                        "topic": "orders",
                        "content": "create login form",
                        "key": "user-auth"
                    }
                }
            }),
        );
        let RpcReply::Json(pub_json) = pub_reply else {
            panic!("expected pub json")
        };
        assert_eq!(pub_json["result"]["isError"], false);
        let pub_data: Value = serde_json::from_str(
            pub_json["result"]["structuredContent"]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(pub_data["topic"], "orders");
        assert_eq!(pub_data["from"], "brain-1");
        assert_eq!(pub_data["offset"], 0);

        // 2. list_topics shows "orders" with 1 message
        let list_reply = rpc(
            &state,
            "brain-token",
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "list_topics",
                    "arguments": {}
                }
            }),
        );
        let RpcReply::Json(list_json) = list_reply else {
            panic!("expected list json")
        };
        assert_eq!(list_json["result"]["isError"], false);
        let list_data: Value = serde_json::from_str(
            list_json["result"]["structuredContent"]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let topics = list_data["topics"].as_array().unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0]["name"], "orders");
        assert_eq!(topics[0]["messages_count"], 1);

        // 3. coder-1 subscribes to "orders" in group "workers"
        let sub_reply = rpc(
            &state,
            "coder-token",
            &json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "subscribe",
                    "arguments": {
                        "topic": "orders",
                        "group": "workers"
                    }
                }
            }),
        );
        let RpcReply::Json(sub_json) = sub_reply else {
            panic!("expected sub json")
        };
        assert_eq!(sub_json["result"]["isError"], false);
        let sub_data: Value = serde_json::from_str(
            sub_json["result"]["structuredContent"]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(sub_data["status"], "subscribed");
        assert_eq!(sub_data["group"], "workers");
        assert_eq!(sub_data["agent"], "coder-1");

        // 4. coder-1 polls "orders" and receives the message
        let poll_reply = rpc(
            &state,
            "coder-token",
            &json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "poll",
                    "arguments": {
                        "topic": "orders"
                    }
                }
            }),
        );
        let RpcReply::Json(poll_json) = poll_reply else {
            panic!("expected poll json")
        };
        assert_eq!(poll_json["result"]["isError"], false);
        let poll_data: Value = serde_json::from_str(
            poll_json["result"]["structuredContent"]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let msgs = poll_data["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "create login form");

        // 5. coder-1 acks offset 0
        let ack_reply = rpc(
            &state,
            "coder-token",
            &json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "ack",
                    "arguments": {
                        "topic": "orders",
                        "offset": 0
                    }
                }
            }),
        );
        let RpcReply::Json(ack_json) = ack_reply else {
            panic!("expected ack json")
        };
        assert_eq!(ack_json["result"]["isError"], false);

        // Next poll returns no messages
        let poll2_reply = rpc(
            &state,
            "coder-token",
            &json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {
                    "name": "poll",
                    "arguments": {
                        "topic": "orders"
                    }
                }
            }),
        );
        let RpcReply::Json(poll2_json) = poll2_reply else {
            panic!("expected poll2 json")
        };
        let poll2_data: Value = serde_json::from_str(
            poll2_json["result"]["structuredContent"]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert!(poll2_data["messages"].as_array().unwrap().is_empty());

        // 6. replay from offset 0 retrieves historical messages without advancing consumer offsets
        let replay_reply = rpc(
            &state,
            "coder-token",
            &json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "replay",
                    "arguments": {
                        "topic": "orders",
                        "from_offset": 0
                    }
                }
            }),
        );
        let RpcReply::Json(replay_json) = replay_reply else {
            panic!("expected replay json")
        };
        let replay_data: Value = serde_json::from_str(
            replay_json["result"]["structuredContent"]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(replay_data["messages"].as_array().unwrap().len(), 1);

        std::fs::remove_dir_all(root).unwrap();
    }
}
