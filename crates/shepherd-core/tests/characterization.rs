//! Characterization test suite for Shepherd Core (Phase F9a).
//!
//! Freezes current JSON outputs for:
//! 1. All `handle_call` operations in `service.rs`.
//! 2. All CLI subcommands supporting `--json` in `cli.rs`.
//! 3. Main MCP tools and error responses in `mcp.rs`.
//!
//! Outputs are normalized (timestamps, paths, tokens, ports) and compared
//! against versioned golden files under `tests/golden/*.json`.
//!
//! To regenerate all golden files explicitly, run:
//! ```bash
//! SHEPHERD_UPDATE_GOLDEN=1 ./cargo-fork.sh test -p shepherd-core --test characterization
//! ```

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use shepherd_core::bus::MessageBus;
use shepherd_core::cli::run_cli_in_root_with_output;
use shepherd_core::config::Config;
use shepherd_core::mcp::RunningMcpServer;
use shepherd_core::service::{ManualClock, OrchestrationService, ServiceConfig};
use shepherd_core::terminals::InMemoryTerminals;
use shepherd_core::{
    create_task, load_agent_registry, save_agent_registry, Agent, AgentRegistryFile, NewTask,
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);
static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub struct TestEnv {
    pub dir: PathBuf,
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub service: OrchestrationService,
    pub terminals: InMemoryTerminals,
    pub clock: ManualClock,
    _guard: std::sync::MutexGuard<'static, ()>,
    orig_xdg_state: Option<std::ffi::OsString>,
    orig_xdg_config: Option<std::ffi::OsString>,
    orig_home: Option<std::ffi::OsString>,
}

impl TestEnv {
    pub fn new(prefix: &str) -> Self {
        let guard = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let orig_xdg_state = std::env::var_os("XDG_STATE_HOME");
        let orig_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
        let orig_home = std::env::var_os("HOME");

        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("shep-f9a-{prefix}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let xdg_state = dir.join("xdg_state");
        let xdg_config = dir.join("xdg_config");
        let home_dir = dir.join("home");
        fs::create_dir_all(&xdg_state).unwrap();
        fs::create_dir_all(&xdg_config).unwrap();
        fs::create_dir_all(&home_dir).unwrap();

        std::env::set_var("XDG_STATE_HOME", &xdg_state);
        std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
        std::env::set_var("HOME", &home_dir);

        let state_dir = xdg_state.join("shepherd");
        fs::create_dir_all(&state_dir).unwrap();

        let root = dir.join("repo");
        fs::create_dir_all(&root).unwrap();

        // Initialize a clean isolated git repository
        let git_run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("git execution");
            assert!(output.status.success(), "git failed: {:?}", args);
        };
        git_run(&["init", "-b", "master"]);
        git_run(&["config", "user.email", "shepherd-test@example.invalid"]);
        git_run(&["config", "user.name", "Shepherd Characterization Test"]);
        fs::write(root.join("README.md"), "# Test Repo\n").unwrap();
        git_run(&["add", "README.md"]);
        git_run(&["commit", "-m", "initial commit"]);

        // Write a standard .shepherd/config.yaml in the project root
        let shepherd_cfg_dir = root.join(".shepherd");
        fs::create_dir_all(&shepherd_cfg_dir).unwrap();
        fs::write(
            shepherd_cfg_dir.join("config.yaml"),
            "workflow:\n  base_branch: master\nlimits:\n  max_depth: 3\n  max_concurrent: 5\n",
        )
        .unwrap();

        let terminals = InMemoryTerminals::new();
        let clock = ManualClock::new();
        let svc_cfg = ServiceConfig::for_state_dir(&state_dir);
        let shep_cfg = Config::default();
        let service = OrchestrationService::new(
            svc_cfg,
            shep_cfg,
            Arc::new(terminals.clone()),
            Arc::new(clock.clone()),
        );

        Self {
            dir,
            root,
            state_dir,
            service,
            terminals,
            clock,
            _guard: guard,
            orig_xdg_state,
            orig_xdg_config,
            orig_home,
        }
    }

    pub fn seed_agent(&self, id: &str, role: &str, state: &str) -> Agent {
        let reg_path = self.state_dir.join("agents.json");
        let mut reg = load_agent_registry(&reg_path).unwrap_or_default();
        let agent = Agent {
            id: id.to_string(),
            role: role.to_string(),
            task_text: format!("Task for {id}"),
            task_id: "1".to_string(),
            root: self.root.display().to_string(),
            state: state.to_string(),
            pane: format!("pane-{id}"),
            ..Default::default()
        };
        let token = "a".repeat(64);
        reg.create(agent.clone(), token, "2026-11-05T00:00:00Z");
        save_agent_registry(&reg_path, &reg).unwrap();
        agent
    }

    pub fn seed_task(&self, id: u32, title: &str) {
        let task = NewTask {
            id,
            title: title.to_string(),
            description: "Sample test task description".to_string(),
            criteria: vec!["Criterion 1".to_string(), "Criterion 2".to_string()],
            issue: None,
            issue_markdown: None,
            links: vec![],
        };
        create_task(&self.root, task).unwrap();
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        match &self.orig_xdg_state {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        match &self.orig_xdg_config {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match &self.orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

pub fn normalize_json(val: &Value, env: &TestEnv) -> Value {
    let test_dir_str = env.dir.display().to_string();
    let root_str = env.root.display().to_string();
    let state_str = env.state_dir.display().to_string();
    let temp_str = std::env::temp_dir().display().to_string();

    normalize_recursive(val, &test_dir_str, &root_str, &state_str, &temp_str)
}

fn normalize_recursive(
    val: &Value,
    test_dir: &str,
    root: &str,
    state_dir: &str,
    temp_dir: &str,
) -> Value {
    match val {
        Value::Null | Value::Bool(_) | Value::Number(_) => val.clone(),
        Value::String(s) => {
            // Check if string contains serialized JSON (e.g. MCP responses)
            if (s.starts_with('{') || s.starts_with('[')) && s.contains('"') {
                if let Ok(nested_val) = serde_json::from_str::<Value>(s) {
                    let normalized_nested =
                        normalize_recursive(&nested_val, test_dir, root, state_dir, temp_dir);
                    return Value::String(serde_json::to_string(&normalized_nested).unwrap());
                }
            }

            let mut s = s.clone();

            // Replace exact test paths
            s = s.replace(root, "<ROOT>");
            s = s.replace(state_dir, "<STATE_DIR>");
            s = s.replace(test_dir, "<TEST_DIR>");
            if !temp_dir.is_empty() {
                s = s.replace(temp_dir, "<TEMP_DIR>");
            }

            // ISO-8601 timestamps
            if s.len() >= 19
                && s.chars().nth(4) == Some('-')
                && s.chars().nth(7) == Some('-')
                && s.chars().nth(10) == Some('T')
            {
                return Value::String("<TIMESTAMP>".to_string());
            }

            // 64-char hex tokens
            if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                return Value::String("<AGENT_TOKEN>".to_string());
            }

            // Localhost MCP URLs
            if s.starts_with("http://127.0.0.1:") && s.ends_with("/mcp") {
                return Value::String("http://127.0.0.1:<PORT>/mcp".to_string());
            }

            Value::String(s)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|item| normalize_recursive(item, test_dir, root, state_dir, temp_dir))
                .collect(),
        ),
        Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                if (k == "ts"
                    || k == "created_at"
                    || k == "updated_at"
                    || k == "started_at"
                    || k == "finished_at")
                    && v.is_string()
                {
                    new_map.insert(k.clone(), Value::String("<TIMESTAMP>".to_string()));
                } else if k == "token" && v.is_string() {
                    new_map.insert(k.clone(), Value::String("<AGENT_TOKEN>".to_string()));
                } else if k == "mcp_url" && v.is_string() {
                    new_map.insert(
                        k.clone(),
                        Value::String("http://127.0.0.1:<PORT>/mcp".to_string()),
                    );
                } else if k == "gh_available" {
                    new_map.insert(k.clone(), Value::Bool(false));
                } else if k == "gh_note" && v.is_string() {
                    new_map.insert(k.clone(), Value::String("<GH_NOTE>".to_string()));
                } else if k == "global" && v.is_string() {
                    new_map.insert(k.clone(), Value::String("<GLOBAL_CONFIG_PATH>".to_string()));
                } else if k == "state" && v.is_string() {
                    new_map.insert(k.clone(), Value::String("<STATE_DIR_PATH>".to_string()));
                } else {
                    new_map.insert(
                        k.clone(),
                        normalize_recursive(v, test_dir, root, state_dir, temp_dir),
                    );
                }
            }
            Value::Object(new_map)
        }
    }
}

pub fn assert_golden(test_name: &str, actual: &Value) {
    let golden_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden");
    fs::create_dir_all(&golden_dir).unwrap();
    let golden_file = golden_dir.join(format!("{test_name}.json"));

    let actual_pretty = format!("{}\n", serde_json::to_string_pretty(actual).unwrap());

    if std::env::var_os("SHEPHERD_UPDATE_GOLDEN").is_some()
        || std::env::var_os("UPDATE_GOLDEN").is_some()
    {
        fs::write(&golden_file, &actual_pretty).unwrap();
    } else {
        if !golden_file.exists() {
            panic!(
                "Golden file does not exist: {}. Run with SHEPHERD_UPDATE_GOLDEN=1 to create it.",
                golden_file.display()
            );
        }
        let expected = fs::read_to_string(&golden_file).unwrap();
        assert_eq!(
            actual_pretty,
            expected,
            "\n\nGolden file mismatch for '{}'!\nFile: {}\nRun with SHEPHERD_UPDATE_GOLDEN=1 to update golden files if this change is intentional.\n",
            test_name,
            golden_file.display()
        );
    }
}

fn run_cli_json(env: &TestEnv, args: &[&str]) -> Result<Value, String> {
    let mut output = Vec::new();
    let args_vec: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let res = run_cli_in_root_with_output(
        env.root.clone(),
        args_vec,
        Some(Arc::new(env.terminals.clone())),
        None,
        &mut output,
    );
    match res {
        Ok(_) => {
            let val = serde_json::from_slice(&output).map_err(|e| {
                format!(
                    "failed to parse CLI JSON output: {e}\nRaw: {}",
                    String::from_utf8_lossy(&output)
                )
            })?;
            Ok(val)
        }
        Err(err) => {
            if let Ok(val) = serde_json::from_slice::<Value>(&output) {
                Ok(val)
            } else {
                Err(err.to_string())
            }
        }
    }
}

fn mcp_request(url: &str, token: &str, body: &Value) -> Result<Value, String> {
    let host = url.trim_start_matches("http://");
    let addr = host.trim_end_matches("/mcp");
    let mut client = TcpStream::connect(addr).map_err(|e| format!("connect failed: {e}"))?;
    let body_str = serde_json::to_string(body).unwrap();
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        token,
        body_str.len(),
        body_str
    );
    client
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;

    let mut response_bytes = Vec::new();
    client
        .read_to_end(&mut response_bytes)
        .map_err(|e| e.to_string())?;
    let response_str = String::from_utf8_lossy(&response_bytes);
    if let Some(pos) = response_str.find("\r\n\r\n") {
        let status_line = response_str.lines().next().unwrap_or_default();
        let json_part = &response_str[pos + 4..];
        if status_line.contains("200") {
            serde_json::from_str(json_part)
                .map_err(|e| format!("invalid json: {e} (raw: {json_part})"))
        } else {
            Ok(json!({
                "http_status": status_line,
                "body": json_part.trim()
            }))
        }
    } else {
        Err(format!("invalid HTTP response: {response_str}"))
    }
}

// =========================================================================
// 1. Tests for `handle_call` operations
// =========================================================================

#[test]
fn test_characterization_handle_call_agents() {
    let env = TestEnv::new("hc-agents");
    env.seed_agent("coder-1", "coder", "idle");
    let res = env.service.handle_call("agents", json!({})).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_agents", &norm);
}

#[test]
fn test_characterization_handle_call_spawn() {
    let env = TestEnv::new("hc-spawn");
    let res = env
        .service
        .handle_call(
            "spawn",
            json!({
                "root": env.root.display().to_string(),
                "role": "coder",
                "task": "Build feature X"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_spawn", &norm);
}

#[test]
fn test_characterization_handle_call_kill() {
    let env = TestEnv::new("hc-kill");
    let agent = env.seed_agent("coder-1", "coder", "working");
    let res = env
        .service
        .handle_call("kill", json!({"id": agent.id}))
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_kill", &norm);
}

#[test]
fn test_characterization_handle_call_clean() {
    let env = TestEnv::new("hc-clean");
    let spawn_res = env
        .service
        .handle_call(
            "spawn",
            json!({
                "root": env.root.display().to_string(),
                "role": "coder",
                "task": "Clean test task"
            }),
        )
        .unwrap();
    let id = spawn_res["id"].as_str().unwrap();
    env.service.handle_call("kill", json!({"id": id})).unwrap();

    let res = env
        .service
        .handle_call("clean", json!({"ids": [id]}))
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_clean", &norm);
}

#[test]
fn test_characterization_handle_call_focus() {
    let env = TestEnv::new("hc-focus");
    let spawn_res = env
        .service
        .handle_call(
            "spawn",
            json!({
                "root": env.root.display().to_string(),
                "role": "coder",
                "task": "Focus test task"
            }),
        )
        .unwrap();
    let id = spawn_res["id"].as_str().unwrap();

    let res = env.service.handle_call("focus", json!({"id": id})).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_focus", &norm);
}

#[test]
fn test_characterization_handle_call_run() {
    let env = TestEnv::new("hc-run");
    env.seed_task(1, "Run workflow task");
    let res = env
        .service
        .handle_call(
            "run",
            json!({
                "root": env.root.display().to_string(),
                "task": "1-run-workflow-task"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_run", &norm);
}

#[test]
fn test_characterization_handle_call_approve() {
    let env = TestEnv::new("hc-approve");
    env.seed_task(1, "Approve task");
    let mut task = shepherd_core::find_task(&env.root, "1").unwrap();
    task.status.waiting = "plan".into();
    fs::write(
        task.directory.join("plan.md"),
        "# Plan\nVerified plan content.\n",
    )
    .unwrap();
    shepherd_core::save_task_status(task.directory.join("status.json"), &task.status).unwrap();

    let res = env
        .service
        .handle_call(
            "approve",
            json!({
                "root": env.root.display().to_string(),
                "task": "1",
                "target": "plan"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_approve", &norm);
}

#[test]
fn test_characterization_handle_call_reject() {
    let env = TestEnv::new("hc-reject");
    env.seed_task(1, "Reject task");
    let mut task = shepherd_core::find_task(&env.root, "1").unwrap();
    task.status.waiting = "plan".into();
    fs::write(task.directory.join("plan.md"), "# Plan\nDraft plan.\n").unwrap();
    shepherd_core::save_task_status(task.directory.join("status.json"), &task.status).unwrap();

    let res = env
        .service
        .handle_call(
            "reject",
            json!({
                "root": env.root.display().to_string(),
                "task": "1",
                "target": "plan",
                "reason": "Needs better verification steps"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_reject", &norm);
}

#[test]
fn test_characterization_handle_call_reject_gate() {
    let env = TestEnv::new("hc-reject-gate");
    env.seed_task(1, "Reject gate task");
    let mut task = shepherd_core::find_task(&env.root, "1").unwrap();
    task.status.waiting = "plan".into();
    fs::write(task.directory.join("plan.md"), "# Plan\nDraft plan.\n").unwrap();
    shepherd_core::save_task_status(task.directory.join("status.json"), &task.status).unwrap();

    let res = env
        .service
        .handle_call(
            "reject_gate",
            json!({
                "root": env.root.display().to_string(),
                "task": "1",
                "target": "plan",
                "reason": "Alias reject_gate test"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_reject_gate", &norm);
}

#[test]
fn test_characterization_handle_call_reconcile() {
    let env = TestEnv::new("hc-reconcile");
    env.seed_agent("coder-1", "coder", "working");
    let res = env.service.handle_call("reconcile", json!({})).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_reconcile", &norm);
}

#[test]
fn test_characterization_handle_call_resume() {
    let env = TestEnv::new("hc-resume");
    env.seed_agent("coder-1", "coder", "starting");
    let res = env.service.handle_call("resume", json!({})).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_resume", &norm);
}

#[test]
fn test_characterization_handle_call_tasks() {
    let env = TestEnv::new("hc-tasks");
    env.seed_task(1, "First Task");
    env.seed_task(2, "Second Task");
    let res = env
        .service
        .handle_call(
            "tasks",
            json!({
                "root": env.root.display().to_string(),
                "archived": false
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_tasks", &norm);
}

#[test]
fn test_characterization_handle_call_task_create() {
    let env = TestEnv::new("hc-task-create");
    let res = env
        .service
        .handle_call(
            "task_create",
            json!({
                "root": env.root.display().to_string(),
                "title": "Created via handle_call",
                "description": "Task creation description",
                "criteria": ["Must pass characterization test"]
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_task_create", &norm);
}

#[test]
fn test_characterization_handle_call_create_task() {
    let env = TestEnv::new("hc-create-task");
    let res = env
        .service
        .handle_call(
            "create_task",
            json!({
                "root": env.root.display().to_string(),
                "title": "Created via create_task alias",
                "description": "Alias creation description",
                "criteria": ["Check alias"]
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_create_task", &norm);
}

#[test]
fn test_characterization_handle_call_task_archive() {
    let env = TestEnv::new("hc-task-archive");
    env.seed_task(1, "To be archived");
    let res = env
        .service
        .handle_call(
            "task_archive",
            json!({
                "root": env.root.display().to_string(),
                "id": 1
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_task_archive", &norm);
}

#[test]
fn test_characterization_handle_call_archive_task() {
    let env = TestEnv::new("hc-archive-task");
    env.seed_task(1, "To be archived alias");
    let res = env
        .service
        .handle_call(
            "archive_task",
            json!({
                "root": env.root.display().to_string(),
                "id": 1
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_archive_task", &norm);
}

#[test]
fn test_characterization_handle_call_skills() {
    let env = TestEnv::new("hc-skills");
    let res = env
        .service
        .handle_call(
            "skills",
            json!({
                "root": env.root.display().to_string()
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_skills", &norm);
}

#[test]
fn test_characterization_handle_call_patterns() {
    let env = TestEnv::new("hc-patterns");
    let res = env
        .service
        .handle_call(
            "patterns",
            json!({
                "root": env.root.display().to_string()
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_patterns", &norm);
}

#[test]
fn test_characterization_handle_call_pattern_get() {
    let env = TestEnv::new("hc-pattern-get");
    let res = env
        .service
        .handle_call(
            "pattern_get",
            json!({
                "root": env.root.display().to_string(),
                "name": "orchestrator-worker"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_pattern_get", &norm);
}

#[test]
fn test_characterization_handle_call_pattern_save() {
    let env = TestEnv::new("hc-pattern-save");
    let pattern_val = json!({
        "name": "saved-pattern",
        "kind": "orchestrator-worker",
        "description": "A saved pattern test",
        "topics": [
            { "name": "orders", "partitions": 1 },
            { "name": "results", "partitions": 1 }
        ],
        "agents": [
            {
                "id": "lead",
                "role": "brain",
                "publishes": ["orders"],
                "consumes": ["results"]
            },
            {
                "id": "worker",
                "role": "coder",
                "consumes": ["orders"],
                "publishes": ["results"],
                "group": "workers"
            }
        ]
    });
    let res = env
        .service
        .handle_call(
            "pattern_save",
            json!({
                "root": env.root.display().to_string(),
                "pattern": pattern_val,
                "global": false
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_pattern_save", &norm);
}

#[test]
fn test_characterization_handle_call_pattern_delete() {
    let env = TestEnv::new("hc-pattern-del");
    let pattern_val = json!({
        "name": "to-delete",
        "kind": "orchestrator-worker",
        "description": "Pattern to delete",
        "topics": [
            { "name": "orders", "partitions": 1 },
            { "name": "results", "partitions": 1 }
        ],
        "agents": [
            {
                "id": "lead",
                "role": "brain",
                "publishes": ["orders"],
                "consumes": ["results"]
            },
            {
                "id": "worker",
                "role": "coder",
                "consumes": ["orders"],
                "publishes": ["results"],
                "group": "workers"
            }
        ]
    });
    env.service
        .handle_call(
            "pattern_save",
            json!({
                "root": env.root.display().to_string(),
                "pattern": pattern_val
            }),
        )
        .unwrap();

    let res = env
        .service
        .handle_call(
            "pattern_delete",
            json!({
                "root": env.root.display().to_string(),
                "name": "to-delete"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_pattern_delete", &norm);
}

#[test]
fn test_characterization_handle_call_pattern_validate() {
    let env = TestEnv::new("hc-pattern-val");
    let res = env
        .service
        .handle_call(
            "pattern_validate",
            json!({
                "root": env.root.display().to_string(),
                "name": "orchestrator-worker"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_pattern_validate", &norm);
}

#[test]
fn test_characterization_handle_call_runs() {
    let env = TestEnv::new("hc-runs");
    env.seed_task(1, "Runs progress task");
    let res = env
        .service
        .handle_call(
            "runs",
            json!({
                "root": env.root.display().to_string()
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_runs", &norm);
}

#[test]
fn test_characterization_handle_call_git_status() {
    let env = TestEnv::new("hc-git-status");
    let res = env
        .service
        .handle_call(
            "git_status",
            json!({
                "root": env.root.display().to_string()
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_git_status", &norm);
}

#[test]
fn test_characterization_handle_call_topics() {
    let env = TestEnv::new("hc-topics");
    let res = env.service.handle_call("topics", json!({})).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_topics", &norm);
}

#[test]
fn test_characterization_handle_call_topic_create() {
    let env = TestEnv::new("hc-topic-create");
    let res = env
        .service
        .handle_call(
            "topic_create",
            json!({
                "name": "characterization-topic",
                "partitions": 3
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_topic_create", &norm);
}

#[test]
fn test_characterization_handle_call_publish() {
    let env = TestEnv::new("hc-publish");
    env.service
        .handle_call(
            "topic_create",
            json!({
                "name": "pub-topic",
                "partitions": 2
            }),
        )
        .unwrap();

    let res = env
        .service
        .handle_call(
            "publish",
            json!({
                "topic": "pub-topic",
                "content": "Characterization message payload",
                "key": "user-42",
                "from": "tester"
            }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_publish", &norm);
}

#[test]
fn test_characterization_handle_call_topic_tail() {
    let env = TestEnv::new("hc-topic-tail");
    env.service
        .handle_call(
            "topic_create",
            json!({ "name": "tail-topic", "partitions": 1 }),
        )
        .unwrap();
    env.service
        .handle_call(
            "publish",
            json!({ "topic": "tail-topic", "content": "tail msg 1", "from": "user" }),
        )
        .unwrap();

    let res = env
        .service
        .handle_call("topic_tail", json!({ "topic": "tail-topic" }))
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_topic_tail", &norm);
}

#[test]
fn test_characterization_handle_call_topic_replay() {
    let env = TestEnv::new("hc-topic-replay");
    env.service
        .handle_call(
            "topic_create",
            json!({ "name": "replay-topic", "partitions": 1 }),
        )
        .unwrap();
    env.service
        .handle_call(
            "publish",
            json!({ "topic": "replay-topic", "content": "replay msg 1", "from": "user" }),
        )
        .unwrap();

    let res = env
        .service
        .handle_call(
            "topic_replay",
            json!({ "topic": "replay-topic", "from_offset": 0 }),
        )
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_topic_replay", &norm);
}

#[test]
fn test_characterization_handle_call_consumer_groups() {
    let env = TestEnv::new("hc-consumer-groups");
    env.service
        .handle_call(
            "topic_create",
            json!({ "name": "groups-topic", "partitions": 1 }),
        )
        .unwrap();
    let bus_path = env.state_dir.join("messages.jsonl");
    let bus = MessageBus::open(&bus_path).unwrap();
    bus.subscribe("agent-1", "groups-topic", Some("group-alpha"))
        .unwrap();

    let res = env
        .service
        .handle_call("consumer_groups", json!({ "topic": "groups-topic" }))
        .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("handle_call_consumer_groups", &norm);
}

#[test]
fn test_characterization_handle_call_unknown_op_error() {
    let env = TestEnv::new("hc-unknown-op");
    let err = env
        .service
        .handle_call("nonexistent_operation", json!({}))
        .unwrap_err();
    let val = json!({ "error": err });
    let norm = normalize_json(&val, &env);
    assert_golden("handle_call_unknown_op_error", &norm);
}

#[test]
fn test_characterization_handle_call_kill_missing_id_error() {
    let env = TestEnv::new("hc-kill-missing-id");
    let err = env.service.handle_call("kill", json!({})).unwrap_err();
    let val = json!({ "error": err });
    let norm = normalize_json(&val, &env);
    assert_golden("handle_call_kill_missing_id_error", &norm);
}

#[test]
fn test_characterization_handle_call_spawn_invalid_args_error() {
    let env = TestEnv::new("hc-spawn-invalid");
    let err = env
        .service
        .handle_call("spawn", json!({"role": 12345}))
        .unwrap_err();
    let val = json!({ "error": err });
    let norm = normalize_json(&val, &env);
    assert_golden("handle_call_spawn_invalid_args_error", &norm);
}

#[test]
fn test_characterization_handle_call_pattern_validation_error() {
    let env = TestEnv::new("hc-pattern-invalid");
    let invalid_pattern = json!({
        "name": "broken_pattern",
        "kind": "pipeline",
        "description": "Broken pattern without required fields"
    });
    let err = env
        .service
        .handle_call(
            "pattern_save",
            json!({
                "root": env.root.display().to_string(),
                "pattern": invalid_pattern
            }),
        )
        .unwrap_err();
    let val = json!({ "error": err });
    let norm = normalize_json(&val, &env);
    assert_golden("handle_call_pattern_validation_error", &norm);
}

// =========================================================================
// 2. Tests for CLI subcommands with --json
// =========================================================================

#[test]
fn test_characterization_cli_agents() {
    let env = TestEnv::new("cli-agents");
    env.seed_agent("coder-1", "coder", "idle");
    let res = run_cli_json(&env, &["agents", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_agents", &norm);
}

#[test]
fn test_characterization_cli_spawn() {
    let env = TestEnv::new("cli-spawn");
    let res = run_cli_json(&env, &["spawn", "coder", "Implement feature Y", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_spawn", &norm);
}

#[test]
fn test_characterization_cli_kill() {
    let env = TestEnv::new("cli-kill");
    let agent = env.seed_agent("coder-1", "coder", "working");
    let res = run_cli_json(&env, &["kill", &agent.id, "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_kill", &norm);
}

#[test]
fn test_characterization_cli_clean() {
    let env = TestEnv::new("cli-clean");
    let agent = env.seed_agent("coder-1", "coder", "killed");
    let res = run_cli_json(&env, &["clean", &agent.id, "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_clean", &norm);
}

#[test]
fn test_characterization_cli_focus() {
    let env = TestEnv::new("cli-focus");
    let agent = env.seed_agent("coder-1", "coder", "idle");
    let res = run_cli_json(&env, &["focus", &agent.id, "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_focus", &norm);
}

#[test]
fn test_characterization_cli_task_new() {
    let env = TestEnv::new("cli-task-new");
    let res = run_cli_json(&env, &["task", "new", "CLI Task Title", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_task_new", &norm);
}

#[test]
fn test_characterization_cli_task_list() {
    let env = TestEnv::new("cli-task-list");
    env.seed_task(1, "CLI Task 1");
    let res = run_cli_json(&env, &["task", "list", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_task_list", &norm);
}

#[test]
fn test_characterization_cli_tasks() {
    let env = TestEnv::new("cli-tasks");
    env.seed_task(1, "Tasks Alias 1");
    let res = run_cli_json(&env, &["tasks", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_tasks", &norm);
}

#[test]
fn test_characterization_cli_task_show() {
    let env = TestEnv::new("cli-task-show");
    env.seed_task(1, "Task To Show");
    let res = run_cli_json(&env, &["task", "show", "1", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_task_show", &norm);
}

#[test]
fn test_characterization_cli_task_plan() {
    let env = TestEnv::new("cli-task-plan");
    env.seed_task(1, "Task For Plan");
    let task_dir = env
        .root
        .join(".shepherd")
        .join("tasks")
        .join("1-task-for-plan");
    fs::write(
        task_dir.join("plan.md"),
        "# Plan\n### 1. Subtask 1\nVerify: cargo check\nDepends: \n",
    )
    .unwrap();
    let res = run_cli_json(&env, &["task", "plan", "1", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_task_plan", &norm);
}

#[test]
fn test_characterization_cli_task_path() {
    let env = TestEnv::new("cli-task-path");
    env.seed_task(1, "Path Task");
    let res = run_cli_json(&env, &["task", "path", "1", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_task_path", &norm);
}

#[test]
fn test_characterization_cli_task_archive() {
    let env = TestEnv::new("cli-task-archive");
    env.seed_task(1, "Archive CLI Task");
    let res = run_cli_json(&env, &["task", "archive", "1", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_task_archive", &norm);
}

#[test]
fn test_characterization_cli_skills_list() {
    let env = TestEnv::new("cli-skills-list");
    let res = run_cli_json(&env, &["skills", "list", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_skills_list", &norm);
}

#[test]
fn test_characterization_cli_runs_list() {
    let env = TestEnv::new("cli-runs-list");
    env.seed_task(1, "Runs List Task");
    let res = run_cli_json(&env, &["runs", "list", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_runs_list", &norm);
}

#[test]
fn test_characterization_cli_roles_list() {
    let env = TestEnv::new("cli-roles-list");
    let res = run_cli_json(&env, &["roles", "list", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_roles_list", &norm);
}

#[test]
fn test_characterization_cli_vendors_list() {
    let env = TestEnv::new("cli-vendors-list");
    let res = run_cli_json(&env, &["vendors", "list", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_vendors_list", &norm);
}

#[test]
fn test_characterization_cli_run() {
    let env = TestEnv::new("cli-run");
    env.seed_task(1, "CLI Run Task");
    let res = run_cli_json(&env, &["run", "1", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_run", &norm);
}

#[test]
fn test_characterization_cli_approve() {
    let env = TestEnv::new("cli-approve");
    env.seed_task(1, "CLI Approve Task");
    let task_dir = env
        .root
        .join(".shepherd")
        .join("tasks")
        .join("1-cli-approve-task");
    fs::write(task_dir.join("plan.md"), "# Plan\nVerified.\n").unwrap();
    let res = run_cli_json(&env, &["approve", "1", "plan", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_approve", &norm);
}

#[test]
fn test_characterization_cli_reject() {
    let env = TestEnv::new("cli-reject");
    env.seed_task(1, "CLI Reject Task");
    let task_dir = env
        .root
        .join(".shepherd")
        .join("tasks")
        .join("1-cli-reject-task");
    fs::write(task_dir.join("plan.md"), "# Plan\nDraft.\n").unwrap();
    let res = run_cli_json(
        &env,
        &["reject", "1", "plan", "--reason", "Improve plan", "--json"],
    )
    .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_reject", &norm);
}

#[test]
fn test_characterization_cli_git() {
    let env = TestEnv::new("cli-git");
    let res = run_cli_json(&env, &["git", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_git", &norm);
}

#[test]
fn test_characterization_cli_config_path() {
    let env = TestEnv::new("cli-config-path");
    let res = run_cli_json(&env, &["config", "path", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_config_path", &norm);
}

#[test]
fn test_characterization_cli_config_validate() {
    let env = TestEnv::new("cli-config-validate");
    let res = run_cli_json(&env, &["config", "validate", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_config_validate", &norm);
}

#[test]
fn test_characterization_cli_pattern_list() {
    let env = TestEnv::new("cli-pattern-list");
    let res = run_cli_json(&env, &["pattern", "list", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_pattern_list", &norm);
}

#[test]
fn test_characterization_cli_patterns() {
    let env = TestEnv::new("cli-patterns");
    let res = run_cli_json(&env, &["patterns", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_patterns", &norm);
}

#[test]
fn test_characterization_cli_pattern_show() {
    let env = TestEnv::new("cli-pattern-show");
    let res = run_cli_json(&env, &["pattern", "show", "pipeline", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_pattern_show", &norm);
}

#[test]
fn test_characterization_cli_pattern_new() {
    let env = TestEnv::new("cli-pattern-new");
    let res = run_cli_json(
        &env,
        &[
            "pattern",
            "new",
            "clipattern",
            "--template",
            "pipeline",
            "--json",
        ],
    )
    .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_pattern_new", &norm);
}

#[test]
fn test_characterization_cli_pattern_validate() {
    let env = TestEnv::new("cli-pattern-validate");
    let res = run_cli_json(&env, &["pattern", "validate", "pipeline", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_pattern_validate", &norm);
}

#[test]
fn test_characterization_cli_pattern_delete() {
    let env = TestEnv::new("cli-pattern-delete");
    run_cli_json(
        &env,
        &[
            "pattern",
            "new",
            "todel",
            "--template",
            "pipeline",
            "--json",
        ],
    )
    .unwrap();
    let res = run_cli_json(&env, &["pattern", "delete", "todel", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_pattern_delete", &norm);
}

#[test]
fn test_characterization_cli_topic_list() {
    let env = TestEnv::new("cli-topic-list");
    let res = run_cli_json(&env, &["topic", "list", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_topic_list", &norm);
}

#[test]
fn test_characterization_cli_topics() {
    let env = TestEnv::new("cli-topics");
    let res = run_cli_json(&env, &["topics", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_topics", &norm);
}

#[test]
fn test_characterization_cli_topic_create() {
    let env = TestEnv::new("cli-topic-create");
    let res = run_cli_json(
        &env,
        &["topic", "create", "clitopic", "--partitions", "2", "--json"],
    )
    .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_topic_create", &norm);
}

#[test]
fn test_characterization_cli_topic_publish() {
    let env = TestEnv::new("cli-topic-publish");
    run_cli_json(&env, &["topic", "create", "clitopicpub", "--json"]).unwrap();
    let res = run_cli_json(
        &env,
        &[
            "topic",
            "publish",
            "clitopicpub",
            "message from cli",
            "--key",
            "user-1",
            "--json",
        ],
    )
    .unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_topic_publish", &norm);
}

#[test]
fn test_characterization_cli_topic_tail() {
    let env = TestEnv::new("cli-topic-tail");
    run_cli_json(&env, &["topic", "create", "clitopictail", "--json"]).unwrap();
    run_cli_json(
        &env,
        &["topic", "publish", "clitopictail", "tail msg", "--json"],
    )
    .unwrap();
    let res = run_cli_json(&env, &["topic", "tail", "clitopictail", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_topic_tail", &norm);
}

#[test]
fn test_characterization_cli_topic_replay() {
    let env = TestEnv::new("cli-topic-replay");
    run_cli_json(&env, &["topic", "create", "clitopicreplay", "--json"]).unwrap();
    run_cli_json(
        &env,
        &["topic", "publish", "clitopicreplay", "replay msg", "--json"],
    )
    .unwrap();
    let res = run_cli_json(&env, &["topic", "replay", "clitopicreplay", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_topic_replay", &norm);
}

#[test]
fn test_characterization_cli_topic_groups() {
    let env = TestEnv::new("cli-topic-groups");
    run_cli_json(&env, &["topic", "create", "clitopicgroups", "--json"]).unwrap();
    let bus_path = env.root.join(".shepherd").join("messages.jsonl");
    let bus = MessageBus::open(&bus_path).unwrap();
    bus.subscribe("agent-cli", "clitopicgroups", Some("my-group"))
        .unwrap();

    let res = run_cli_json(&env, &["topic", "groups", "clitopicgroups", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_topic_groups", &norm);
}

#[test]
fn test_characterization_cli_error() {
    let env = TestEnv::new("cli-error");
    let res = run_cli_json(&env, &["task", "show", "99999", "--json"]).unwrap();
    let norm = normalize_json(&res, &env);
    assert_golden("cli_error", &norm);
}

// =========================================================================
// 3. Tests for MCP Tools and Errors
// =========================================================================

struct McpTestRig {
    pub env: TestEnv,
    pub server: RunningMcpServer,
    pub token: String,
}

impl McpTestRig {
    pub fn new() -> Self {
        let env = TestEnv::new("mcp-rig");
        let token = "b".repeat(64);
        let mut reg = AgentRegistryFile::default();
        let agent = Agent {
            id: "coder-1".into(),
            role: "coder".into(),
            root: env.root.display().to_string(),
            task_text: "Sample task".into(),
            task_id: "1".into(),
            pane: "pane-coder-1".into(),
            ..Default::default()
        };
        reg.create(agent, token.clone(), "2026-11-05T00:00:00Z");
        save_agent_registry(env.state_dir.join("agents.json"), &reg).unwrap();

        let server = RunningMcpServer::start(
            0,
            env.state_dir.clone(),
            Some(Arc::new(env.terminals.clone())),
            None,
        )
        .expect("start mcp server");

        Self { env, server, token }
    }
}

impl Drop for McpTestRig {
    fn drop(&mut self) {
        self.server.shutdown();
    }
}

#[test]
fn test_characterization_mcp_initialize() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_initialize", &norm);
}

#[test]
fn test_characterization_mcp_tools_list() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_tools_list", &norm);
}

#[test]
fn test_characterization_mcp_spawn_agent() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "spawn_agent",
            "arguments": {
                "role": "tester",
                "task": "Test sub-feature"
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_spawn_agent", &norm);
}

#[test]
fn test_characterization_mcp_submit_result() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "submit_result",
            "arguments": {
                "summary": "Implementation completed successfully."
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_submit_result", &norm);
}

#[test]
fn test_characterization_mcp_get_result() {
    let rig = McpTestRig::new();
    // Submit result first
    let submit_body = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "submit_result",
            "arguments": { "summary": "Done with unit tests." }
        }
    });
    mcp_request(rig.server.url(), &rig.token, &submit_body).unwrap();

    let body = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "get_result",
            "arguments": {
                "agent_id": "coder-1"
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_get_result", &norm);
}

#[test]
fn test_characterization_mcp_list_agents() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "list_agents",
            "arguments": {}
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_list_agents", &norm);
}

#[test]
fn test_characterization_mcp_read_output() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "read_output",
            "arguments": {
                "agent_id": "coder-1",
                "lines": 10
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_read_output", &norm);
}

#[test]
fn test_characterization_mcp_get_task_context() {
    let rig = McpTestRig::new();
    rig.env.seed_task(1, "Task For Context");
    let body = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
        "params": {
            "name": "get_task_context",
            "arguments": {}
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_get_task_context", &norm);
}

#[test]
fn test_characterization_mcp_send_message() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": {
            "name": "send_message",
            "arguments": {
                "to": "user",
                "content": "Hello from MCP coder agent"
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_send_message", &norm);
}

#[test]
fn test_characterization_mcp_read_messages() {
    let rig = McpTestRig::new();
    // Send a message first
    let bus_path = rig.env.state_dir.join("messages.jsonl");
    let bus = MessageBus::open(&bus_path).unwrap();
    bus.send(shepherd_core::bus::Message::directed(
        "user",
        "coder-1",
        shepherd_core::bus::KIND_MESSAGE,
        "Instructions from user",
    ))
    .unwrap();

    let body = json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "read_messages",
            "arguments": {}
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_read_messages", &norm);
}

#[test]
fn test_characterization_mcp_get_workspace_instructions() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": {
            "name": "get_workspace_instructions",
            "arguments": {}
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_get_workspace_instructions", &norm);
}

#[test]
fn test_characterization_mcp_publish() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "tools/call",
        "params": {
            "name": "publish",
            "arguments": {
                "topic": "mcp-events",
                "content": "Event payload via MCP"
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_publish", &norm);
}

#[test]
fn test_characterization_mcp_subscribe() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 13,
        "method": "tools/call",
        "params": {
            "name": "subscribe",
            "arguments": {
                "topic": "mcp-events",
                "group": "mcp-workers"
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_subscribe", &norm);
}

#[test]
fn test_characterization_mcp_poll() {
    let rig = McpTestRig::new();
    // Subscribe and publish
    let sub_body = json!({
        "jsonrpc": "2.0", "id": 13, "method": "tools/call",
        "params": { "name": "subscribe", "arguments": { "topic": "poll-topic" } }
    });
    mcp_request(rig.server.url(), &rig.token, &sub_body).unwrap();

    let pub_body = json!({
        "jsonrpc": "2.0", "id": 14, "method": "tools/call",
        "params": { "name": "publish", "arguments": { "topic": "poll-topic", "content": "polled msg" } }
    });
    mcp_request(rig.server.url(), &rig.token, &pub_body).unwrap();

    let body = json!({
        "jsonrpc": "2.0",
        "id": 15,
        "method": "tools/call",
        "params": {
            "name": "poll",
            "arguments": {
                "topic": "poll-topic"
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_poll", &norm);
}

#[test]
fn test_characterization_mcp_ack() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 16,
        "method": "tools/call",
        "params": {
            "name": "ack",
            "arguments": {
                "topic": "poll-topic",
                "partition": 0,
                "offset": 1
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_ack", &norm);
}

#[test]
fn test_characterization_mcp_replay() {
    let rig = McpTestRig::new();
    let pub_body = json!({
        "jsonrpc": "2.0", "id": 17, "method": "tools/call",
        "params": { "name": "publish", "arguments": { "topic": "replay-mcp", "content": "hist 1" } }
    });
    mcp_request(rig.server.url(), &rig.token, &pub_body).unwrap();

    let body = json!({
        "jsonrpc": "2.0",
        "id": 18,
        "method": "tools/call",
        "params": {
            "name": "replay",
            "arguments": {
                "topic": "replay-mcp",
                "from_offset": 0
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_replay", &norm);
}

#[test]
fn test_characterization_mcp_list_topics() {
    let rig = McpTestRig::new();
    let pub_body = json!({
        "jsonrpc": "2.0", "id": 19, "method": "tools/call",
        "params": { "name": "publish", "arguments": { "topic": "listed-topic", "content": "hi" } }
    });
    mcp_request(rig.server.url(), &rig.token, &pub_body).unwrap();

    let body = json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/call",
        "params": {
            "name": "list_topics",
            "arguments": {}
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_list_topics", &norm);
}

#[test]
fn test_characterization_mcp_unknown_tool_error() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 21,
        "method": "tools/call",
        "params": {
            "name": "unknown_tool_name",
            "arguments": {}
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_unknown_tool_error", &norm);
}

#[test]
fn test_characterization_mcp_unauthorized_error() {
    let rig = McpTestRig::new();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 22,
        "method": "tools/list",
        "params": {}
    });
    let res = mcp_request(rig.server.url(), "invalid-token-here", &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_unauthorized_error", &norm);
}

#[test]
fn test_characterization_mcp_permission_error() {
    let rig = McpTestRig::new();
    rig.env.seed_task(1, "Task For Perm");
    // coder is not permitted to update_task_context (only brain or docs)
    let body = json!({
        "jsonrpc": "2.0",
        "id": 23,
        "method": "tools/call",
        "params": {
            "name": "update_task_context",
            "arguments": {
                "section": "spec",
                "content": "Hacked content"
            }
        }
    });
    let res = mcp_request(rig.server.url(), &rig.token, &body).unwrap();
    let norm = normalize_json(&res, &rig.env);
    assert_golden("mcp_permission_error", &norm);
}
