use super::*;
use crate::terminals::{shepherd_attach_args, InMemoryTerminals};
use crate::{find_task, save_task_status, write_task_section};
use serde_json::Value;

static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct TestEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    orig_xdg: Option<std::ffi::OsString>,
    orig_xdg_config: Option<std::ffi::OsString>,
    orig_home: Option<std::ffi::OsString>,
    orig_editor: Option<std::ffi::OsString>,
    dir: PathBuf,
}

impl TestEnvGuard {
    fn new() -> Self {
        let lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let orig_xdg = std::env::var_os("XDG_STATE_HOME");
        let orig_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
        let orig_home = std::env::var_os("HOME");
        let orig_editor = std::env::var_os("EDITOR");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("shepherd-test-env-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(dir.join("shepherd")).unwrap();
        std::env::set_var("XDG_STATE_HOME", &dir);
        std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg"));
        std::env::set_var("HOME", &dir);
        std::env::set_var("EDITOR", "true");
        Self {
            _lock: lock,
            orig_xdg,
            orig_xdg_config,
            orig_home,
            orig_editor,
            dir,
        }
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        match &self.orig_xdg {
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
        match &self.orig_editor {
            Some(v) => std::env::set_var("EDITOR", v),
            None => std::env::remove_var("EDITOR"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn attach_delegates_to_the_selected_shepherd_session_and_agent() {
    assert_eq!(
        shepherd_attach_args("shepherd", Some("coder-1"), true),
        [
            "--session",
            "shepherd",
            "agent",
            "attach",
            "coder-1",
            "--takeover"
        ]
    );
    assert_eq!(
        shepherd_attach_args("-", Some("w1:p2"), false),
        ["agent", "attach", "w1:p2"]
    );
    assert_eq!(
        shepherd_attach_args("shepherd", None, false),
        ["session", "attach", "shepherd"]
    );
    assert!(shepherd_attach_args("-", None, false).is_empty());
}

#[test]
fn plan_command_validates_and_persists_subtasks_in_go_compatible_status() {
    let _env = TestEnvGuard::new();
    use crate::{create_task, find_task, write_task_section, NewTask};
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-plan-command-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let task = create_task(
        &root,
        NewTask {
            id: 1,
            title: "Plan test".into(),
            description: String::new(),
            criteria: Vec::new(),
            issue: None,
            issue_markdown: None,
            links: Vec::new(),
        },
    )
    .unwrap();
    write_task_section(
            &task,
            "plan",
            "# Plan\n\n## Subtask 1: Base\nVerify: cargo test\n\n## Subtask 2: Follow-up\nDepends on: 1\n",
        )
        .unwrap();
    run_with_buf(&root, &["task", "plan", "1", "--save"], None, None).unwrap();
    let saved = find_task(&root, "1").unwrap();
    assert_eq!(saved.status.subtasks.len(), 2);
    assert_eq!(saved.status.subtasks[1].depends_on, [1]);
    assert_eq!(saved.status.subtasks[0].verify, ["cargo test"]);
    assert_eq!(saved.status.state, "planning");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_json_flags_and_injected_terminals_are_accepted() {
    let _env = TestEnvGuard::new();
    use crate::{create_task, NewTask};
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-json-command-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    create_task(
        &root,
        NewTask {
            id: 1,
            title: "JSON CLI test".into(),
            description: "Desc".into(),
            criteria: vec!["Check".into()],
            issue: None,
            issue_markdown: None,
            links: Vec::new(),
        },
    )
    .unwrap();

    run_with_buf(&root, &["agents", "--json"], None, None).unwrap();
    run_with_buf(&root, &["task", "list", "--json"], None, None).unwrap();
    run_with_buf(&root, &["task", "list", "--archived", "--json"], None, None).unwrap();
    run_with_buf(&root, &["task", "show", "1", "--json"], None, None).unwrap();
    run_with_buf(&root, &["task", "show", "1", "spec", "--json"], None, None).unwrap();
    run_with_buf(&root, &["roles", "list", "--json"], None, None).unwrap();

    let fake = Arc::new(InMemoryTerminals::new());
    let terminals_dyn: Arc<dyn Terminals> = Arc::clone(&fake) as Arc<dyn Terminals>;
    run_with_buf(
        &root,
        &["clean", "--json"],
        Some(Arc::clone(&terminals_dyn)),
        None,
    )
    .unwrap();
    run_with_buf(
        &root,
        &["attach", "coder-1", "--takeover"],
        Some(terminals_dyn),
        None,
    )
    .unwrap();

    std::fs::remove_dir_all(root).unwrap();
}

fn run_with_buf(
    root: &Path,
    args: &[&str],
    terminals: Option<Arc<dyn Terminals>>,
    client: Option<ShepherdClient>,
) -> Result<String, (String, Box<dyn std::error::Error + Send + Sync>)> {
    let mut buf = Vec::new();
    let string_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    match run_cli_in_root_with_output(root.to_path_buf(), string_args, terminals, client, &mut buf)
    {
        Ok(()) => Ok(String::from_utf8_lossy(&buf).to_string()),
        Err(e) => Err((String::from_utf8_lossy(&buf).to_string(), e)),
    }
}

#[test]
fn test_flag_parsing_no_server_and_json_position() {
    let _env = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("shepherd-flag-test-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    // 1. Mock server client that tracks whether it was invoked
    let invoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let invoked_clone = Arc::clone(&invoked);
    let client = ShepherdClient::in_process(move |method, params| {
        assert_eq!(method, "shepherd.call");
        invoked_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(params["op"], "agents");
        Ok(serde_json::json!({
            "type": "shepherd_call",
            "data": []
        }))
    });

    // 2. Normal execution without --no-server uses the server
    let out = run_with_buf(&root, &["agents", "--json"], None, Some(client)).unwrap();
    assert!(invoked.load(std::sync::atomic::Ordering::SeqCst));
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert!(parsed.is_array());

    // 3. Execution with --no-server (placed anywhere) bypasses the server
    let invoked_no_srv = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let invoked_no_srv_clone = Arc::clone(&invoked_no_srv);
    let client_no_srv = ShepherdClient::in_process(move |_method, _params| {
        invoked_no_srv_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(serde_json::json!({}))
    });
    let out2 = run_with_buf(
        &root,
        &["--no-server", "agents", "--json"],
        None,
        Some(client_no_srv),
    )
    .unwrap();
    assert!(!invoked_no_srv.load(std::sync::atomic::Ordering::SeqCst));
    let parsed2: Value = serde_json::from_str(&out2).unwrap();
    assert!(parsed2.is_array());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_fallback_to_in_process_when_no_server() {
    let _env = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-fallback-test-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    // Pass a client pointing to a nonexistent socket
    let client = ShepherdClient::new(PathBuf::from("/tmp/nonexistent-shepherd-socket-12345.sock"));
    let out = run_with_buf(&root, &["agents", "--json"], None, Some(client)).unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert!(parsed.is_array());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_json_error_output() {
    let _env = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-json-err-test-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    // 1. Task not found error with --json
    let (out, err) =
        run_with_buf(&root, &["task", "show", "9999", "--json"], None, None).unwrap_err();
    assert!(err.to_string().contains("9999"));
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert!(parsed.get("error").is_some());
    assert!(parsed["error"].as_str().unwrap().contains("9999"));

    // 2. Kill missing id with --json
    let (out_kill, _) = run_with_buf(&root, &["kill", "--json"], None, None).unwrap_err();
    let parsed_kill: Value = serde_json::from_str(&out_kill).unwrap();
    assert!(parsed_kill["error"].as_str().unwrap().contains("usage"));

    // 3. Msg / messages not available error with --json
    let (out_msg, _) = run_with_buf(&root, &["messages", "--json"], None, None).unwrap_err();
    let parsed_msg: Value = serde_json::from_str(&out_msg).unwrap();
    assert!(parsed_msg["error"]
        .as_str()
        .unwrap()
        .contains("operation not available"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_all_subcommands_json_output() {
    let _env = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("shepherd-all-json-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let fake_terminals = Arc::new(InMemoryTerminals::new());
    let terminals_dyn: Arc<dyn Terminals> = Arc::clone(&fake_terminals) as Arc<dyn Terminals>;

    // 1. task new --json
    let out = run_with_buf(
        &root,
        &[
            "task",
            "new",
            "Feature X",
            "--description",
            "My feature",
            "--criteria",
            "- [ ] Must work",
            "--id",
            "1",
            "--json",
        ],
        None,
        None,
    )
    .unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["status"]["id"], 1);
    assert_eq!(v["status"]["title"], "Feature X");
    assert!(v.get("directory").is_some());

    // 2. task list --json
    let out = run_with_buf(&root, &["task", "list", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_array());
    assert_eq!(v[0]["id"], 1);

    // 3. task list --archived --json
    let out = run_with_buf(&root, &["task", "list", "--archived", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_array());

    // 4. task show 1 --json
    let out = run_with_buf(&root, &["task", "show", "1", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], 1);
    assert!(v.get("context").is_some());

    // 5. task show 1 spec --json
    let out = run_with_buf(&root, &["task", "show", "1", "spec", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["section"], "spec");
    assert!(v.get("content").is_some());

    // 6. task plan 1 --json
    let task = find_task(&root, "1").unwrap();
    write_task_section(
        &task,
        "plan",
        "# Plan\n\n## Subtask 1: Base\nVerify: true\n",
    )
    .unwrap();
    let out = run_with_buf(
        &root,
        &["task", "plan", "1", "--save", "--json"],
        None,
        None,
    )
    .unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], 1);
    assert!(v.get("subtasks").is_some());
    assert_eq!(v["saved"], true);

    // 7. task path 1 --json
    let out = run_with_buf(&root, &["task", "path", "1", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("path").is_some());

    // 8. roles list --json
    let out = run_with_buf(&root, &["roles", "list", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_array());

    // 9. vendors list --json
    let out = run_with_buf(&root, &["vendors", "list", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_array());
    assert!(v[0].get("name").is_some());
    assert!(v[0].get("command").is_some());

    // 10. config path --json
    let out = run_with_buf(&root, &["config", "path", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("global").is_some());
    assert!(v.get("project").is_some());
    assert!(v.get("state").is_some());

    // 11. config validate --json
    let out = run_with_buf(&root, &["config", "validate", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("valid").is_some());

    // 12. skills list --json
    let out = run_with_buf(&root, &["skills", "list", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_array());

    // 13. runs list --json
    let out = run_with_buf(&root, &["runs", "list", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_array());
    assert_eq!(v[0]["task_id"], 1);

    // 14. agents --json
    let out = run_with_buf(&root, &["agents", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.is_array());

    // 15. clean --json
    let out = run_with_buf(
        &root,
        &["clean", "--json"],
        Some(Arc::clone(&terminals_dyn)),
        None,
    )
    .unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("cleaned").is_some());

    // 16. approve 1 plan --json
    let mut status = task.status.clone();
    status.waiting = "plan".into();
    save_task_status(task.directory.join("status.json"), &status).unwrap();
    let out = run_with_buf(&root, &["approve", "1", "plan", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["task"], "1");
    assert_eq!(v["approved"], "plan");

    // 16b. reject 1 plan --reason "reformulate" --json
    let mut status = task.status.clone();
    status.waiting = "plan".into();
    save_task_status(task.directory.join("status.json"), &status).unwrap();
    let out = run_with_buf(
        &root,
        &["reject", "1", "plan", "--reason", "reformulate", "--json"],
        None,
        None,
    )
    .unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["task"], "1");
    assert_eq!(v["rejected"], "plan");
    assert_eq!(v["reason"], "reformulate");

    // 17. task archive 1 --json
    let out = run_with_buf(&root, &["task", "archive", "1", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["archived"], true);
    assert!(v.get("path").is_some());

    // 18. git --json
    let _ = std::process::Command::new("git")
        .args(["init", "-b", "master"])
        .current_dir(&root)
        .output();
    let out = run_with_buf(&root, &["git", "--json"], None, None).unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("worktrees").is_some());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_server_mode_delegation_via_in_process_client() {
    let _env = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-server-delegation-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let ops_called = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ops_clone = Arc::clone(&ops_called);

    let client = ShepherdClient::in_process(move |method, params| {
        assert_eq!(method, "shepherd.call");
        let op = params["op"].as_str().unwrap().to_string();
        ops_clone
            .lock()
            .unwrap()
            .push((op.clone(), params["args"].clone()));
        match op.as_str() {
            "agents" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": [
                    { "id": "coder-1", "state": "idle", "role": "coder", "vendor": "gemini", "model": "", "task_text": "demo" }
                ]
            })),
            "spawn" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "id": "coder-2", "role": "coder", "vendor": "gemini", "model": "", "state": "starting", "pane": "w1:p2", "worktree": "", "branch": "" }
            })),
            "kill" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "id": "coder-1", "state": "killed" }
            })),
            "clean" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "cleaned": ["coder-1"] }
            })),
            "focus" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "id": "coder-1" }
            })),
            "skills" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": [
                    { "name": "test-skill", "tier": "default", "description": "A skill" }
                ]
            })),
            "runs" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": [
                    { "task_id": 1, "task_title": "Run demo", "status": "active", "pending_gate": null, "active_agents": [] }
                ]
            })),
            "tasks" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": [
                    { "status": { "id": 1, "state": "planning", "title": "T1", "slug": "t1", "issue": null, "pr": 0 }, "directory": "/tmp/t1" }
                ]
            })),
            "task_create" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "status": { "id": 2, "state": "draft", "title": "T2", "slug": "t2" }, "directory": "/tmp/t2" }
            })),
            "task_archive" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "archived": true, "path": "/tmp/archive/1" }
            })),
            "run" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "accepted": true, "job": "run-1", "task": "1" }
            })),
            "approve" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "task": "1", "approved": "plan" }
            })),
            "reject" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": { "ok": true, "task": "1", "rejected": "plan" }
            })),
            "git_status" => Ok(serde_json::json!({
                "type": "shepherd_call",
                "data": {
                    "root": "/tmp/repo",
                    "worktrees": [
                        {
                            "path": "/tmp/repo",
                            "branch": "master",
                            "is_main": true,
                            "is_clean": true,
                            "dirty_count": 0,
                            "ahead": 0,
                            "behind": 0,
                            "upstream": null,
                            "agent_id": null,
                            "pr": null
                        }
                    ],
                    "gh_available": false,
                    "gh_note": "gh no disponible",
                    "error": null
                }
            })),
            other => Err(crate::shepherd::ShepherdError::Api {
                code: "unknown_op".into(),
                message: format!("unknown op {other}"),
            }),
        }
    });

    // 1. agents in server mode (human and JSON)
    let out_json = run_with_buf(&root, &["agents", "--json"], None, Some(client.clone())).unwrap();
    let parsed: Value = serde_json::from_str(&out_json).unwrap();
    assert_eq!(parsed[0]["id"], "coder-1");

    let out_human = run_with_buf(&root, &["agents"], None, Some(client.clone())).unwrap();
    assert!(out_human.contains("coder-1"));

    // 2. spawn in server mode
    let out = run_with_buf(
        &root,
        &["spawn", "coder", "do task", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["id"], "coder-2");

    // 3. kill in server mode
    let out = run_with_buf(
        &root,
        &["kill", "coder-1", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["state"], "killed");

    // 4. clean in server mode
    let out = run_with_buf(&root, &["clean", "--json"], None, Some(client.clone())).unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["cleaned"][0], "coder-1");

    // 5. focus in server mode
    let out = run_with_buf(
        &root,
        &["focus", "coder-1", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["id"], "coder-1");

    // 6. skills list in server mode
    let out = run_with_buf(
        &root,
        &["skills", "list", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed[0]["name"], "test-skill");

    // 7. runs list in server mode
    let out = run_with_buf(
        &root,
        &["runs", "list", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed[0]["task_id"], 1);

    // 8. tasks in server mode
    let out = run_with_buf(
        &root,
        &["task", "list", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed[0]["status"]["id"], 1);

    // 9. task new in server mode
    let out = run_with_buf(
        &root,
        &["task", "new", "T2", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["status"]["id"], 2);

    // 10. task archive in server mode
    let out = run_with_buf(
        &root,
        &["task", "archive", "1", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["archived"], true);

    // 11. run in server mode
    let out = run_with_buf(&root, &["run", "1", "--json"], None, Some(client.clone())).unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["accepted"], true);
    assert_eq!(parsed["job"], "run-1");

    // 12. approve in server mode
    let out = run_with_buf(
        &root,
        &["approve", "1", "plan", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["approved"], "plan");

    // 12b. reject in server mode
    let out = run_with_buf(
        &root,
        &["reject", "1", "plan", "--reason", "too complex", "--json"],
        None,
        Some(client.clone()),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["rejected"], "plan");

    // 13. git in server mode
    let out = run_with_buf(&root, &["git", "--json"], None, Some(client.clone())).unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert!(parsed.get("worktrees").is_some());

    // Verify that all 13 alive-state ops were dispatched to shepherd.call!
    let calls = ops_called.lock().unwrap();
    let dispatched_ops: Vec<String> = calls.iter().map(|(op, _)| op.clone()).collect();
    assert!(dispatched_ops.contains(&"agents".to_string()));
    assert!(dispatched_ops.contains(&"spawn".to_string()));
    assert!(dispatched_ops.contains(&"kill".to_string()));
    assert!(dispatched_ops.contains(&"clean".to_string()));
    assert!(dispatched_ops.contains(&"focus".to_string()));
    assert!(dispatched_ops.contains(&"skills".to_string()));
    assert!(dispatched_ops.contains(&"runs".to_string()));
    assert!(dispatched_ops.contains(&"tasks".to_string()));
    assert!(dispatched_ops.contains(&"task_create".to_string()));
    assert!(dispatched_ops.contains(&"task_archive".to_string()));
    assert!(dispatched_ops.contains(&"run".to_string()));
    assert!(dispatched_ops.contains(&"approve".to_string()));
    assert!(dispatched_ops.contains(&"reject".to_string()));
    assert!(dispatched_ops.contains(&"git_status".to_string()));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_server_failure_does_not_fallback_to_in_process() {
    let _env = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-server-fail-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    // Client whose call fails (e.g. server busy, timeout, or internal error)
    let failing_client = ShepherdClient::in_process(|_method, _params| {
        Err(crate::shepherd::ShepherdError::Protocol(
            "server timeout or internal error".into(),
        ))
    });

    // 1. With --json: should exit with error and json output containing {"error": ...}, NOT silent []
    let (out, err) = run_with_buf(
        &root,
        &["agents", "--json"],
        None,
        Some(failing_client.clone()),
    )
    .unwrap_err();

    assert!(err.to_string().contains("server timeout or internal error"));
    let parsed: Value = serde_json::from_str(&out).expect("must output valid error json");
    assert!(parsed.get("error").is_some());
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("server timeout or internal error"));
    // Ensure it did not output silent empty array "[]"
    assert_ne!(out.trim(), "[]");

    // 2. Without --json: should also return Err
    let (_out_human, err_human) =
        run_with_buf(&root, &["agents"], None, Some(failing_client)).unwrap_err();
    assert!(err_human
        .to_string()
        .contains("server timeout or internal error"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_no_server_or_nonexistent_socket_falls_back_to_in_process() {
    let _env = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("shepherd-no-server-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    // 1. Client with a nonexistent socket falls back to in-process mode
    let nonexistent_sock = root.join("nonexistent.sock");
    let client_nonexistent = ShepherdClient::new(nonexistent_sock);
    let out = run_with_buf(&root, &["agents", "--json"], None, Some(client_nonexistent)).unwrap();
    let parsed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed, serde_json::json!([]));

    // 2. Client provided, but --no-server is passed: falls back to in-process mode
    let failing_client = ShepherdClient::in_process(|_method, _params| {
        Err(crate::shepherd::ShepherdError::Protocol(
            "should never be called".into(),
        ))
    });
    let out_no_server = run_with_buf(
        &root,
        &["agents", "--no-server", "--json"],
        None,
        Some(failing_client),
    )
    .unwrap();
    let parsed_no_server: Value = serde_json::from_str(&out_no_server).unwrap();
    assert_eq!(parsed_no_server, serde_json::json!([]));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_pattern_cli_lifecycle_and_json() {
    let _guard = TestEnvGuard::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "shepherd-test-cli-pat-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join(".shepherd")).unwrap();

    // 1. pattern list --json
    let out_list = run_with_buf(&root, &["pattern", "list", "--json"], None, None).unwrap();
    let list_json: Value = serde_json::from_str(&out_list).expect("valid json list");
    let list_arr = list_json.as_array().expect("array of patterns");
    assert_eq!(list_arr.len(), 4);

    // Human list format check
    let out_human_list = run_with_buf(&root, &["pattern", "list"], None, None).unwrap();
    assert!(out_human_list.contains("orchestrator-worker"));
    assert!(out_human_list.contains("hierarchical"));
    assert!(out_human_list.contains("blackboard"));
    assert!(out_human_list.contains("market"));

    // 2. pattern show orchestrator-worker --json
    let out_show = run_with_buf(
        &root,
        &["pattern", "show", "orchestrator-worker", "--json"],
        None,
        None,
    )
    .unwrap();
    let show_json: Value = serde_json::from_str(&out_show).expect("valid json show");
    assert_eq!(show_json["name"], "orchestrator-worker");
    assert_eq!(show_json["kind"], "orchestrator-worker");

    // Human show format check
    let out_human_show = run_with_buf(
        &root,
        &["pattern", "show", "orchestrator-worker"],
        None,
        None,
    )
    .unwrap();
    assert!(out_human_show.contains("Pattern: orchestrator-worker"));
    assert!(out_human_show.contains("orchestrator"));

    // 3. pattern new demo --from orchestrator-worker --json
    let out_new = run_with_buf(
        &root,
        &[
            "pattern",
            "new",
            "demo",
            "--from",
            "orchestrator-worker",
            "--json",
        ],
        None,
        None,
    )
    .unwrap();
    let new_json: Value = serde_json::from_str(&out_new).expect("valid json new");
    assert_eq!(new_json["ok"], true);
    assert_eq!(new_json["name"], "demo");

    let demo_path = root.join(".shepherd/patterns/demo.yaml");
    assert!(demo_path.exists());

    // 4. pattern validate demo --json
    let out_valid = run_with_buf(
        &root,
        &["pattern", "validate", "demo", "--json"],
        None,
        None,
    )
    .unwrap();
    let valid_json: Value = serde_json::from_str(&out_valid).expect("valid json validate");
    assert_eq!(valid_json["valid"], true);

    // 5. pattern validate --file <path> --json
    let out_valid_file = run_with_buf(
        &root,
        &[
            "pattern",
            "validate",
            "--file",
            demo_path.to_str().unwrap(),
            "--json",
        ],
        None,
        None,
    )
    .unwrap();
    let valid_file_json: Value =
        serde_json::from_str(&out_valid_file).expect("valid json validate file");
    assert_eq!(valid_file_json["valid"], true);

    // 6. Break demo.yaml and verify validate catches and explains it
    let mut broken_content = std::fs::read_to_string(&demo_path).unwrap();
    broken_content = broken_content.replace("- orders", "- broken-unknown-orders-topic");
    std::fs::write(&demo_path, broken_content).unwrap();

    // 6a. --json returns valid: false with explanation
    let out_broken = run_with_buf(
        &root,
        &["pattern", "validate", "demo", "--json"],
        None,
        None,
    )
    .unwrap();
    let broken_json: Value = serde_json::from_str(&out_broken).expect("valid json explanation");
    assert_eq!(broken_json["valid"], false);
    let problems = broken_json["problems"].as_array().unwrap();
    assert!(problems.iter().any(|p| p["message"]
        .as_str()
        .map_or(false, |m| m.contains("broken-unknown-orders-topic"))));

    // 6b. human output returns Err and prints diagnostic
    let (_, human_err) =
        run_with_buf(&root, &["pattern", "validate", "demo"], None, None).unwrap_err();
    assert!(human_err.to_string().contains("pattern validation failed"));

    // 7. pattern delete demo --json
    let out_del =
        run_with_buf(&root, &["pattern", "delete", "demo", "--json"], None, None).unwrap();
    let del_json: Value = serde_json::from_str(&out_del).expect("valid json delete");
    assert_eq!(del_json["ok"], true);
    assert_eq!(del_json["deleted_source"], "project");
    assert!(!demo_path.exists());

    // 8. Delete builtin template fails and outputs json error
    let (out_del_err, err_del) = run_with_buf(
        &root,
        &["pattern", "delete", "orchestrator-worker", "--json"],
        None,
        None,
    )
    .unwrap_err();
    assert!(err_del.to_string().contains("cannot delete built-in"));
    let del_err_json: Value = serde_json::from_str(&out_del_err).expect("valid error json");
    assert!(del_err_json["error"]
        .as_str()
        .unwrap()
        .contains("cannot delete built-in"));

    // 9. Pattern edit on project file succeeds (with EDITOR=true)
    let _ = run_with_buf(
        &root,
        &["pattern", "new", "editdemo", "--from", "blackboard"],
        None,
        None,
    )
    .unwrap();
    assert!(run_with_buf(&root, &["pattern", "edit", "editdemo"], None, None).is_ok());

    // 10. Pattern edit on builtin template fails with actionable instruction
    let (_, err_edit_builtin) = run_with_buf(
        &root,
        &["pattern", "edit", "orchestrator-worker"],
        None,
        None,
    )
    .unwrap_err();
    assert!(err_edit_builtin
        .to_string()
        .contains("cannot edit built-in"));

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn test_topic_cli_lifecycle_and_json() {
    let _guard = TestEnvGuard::new();
    let root = std::env::temp_dir().join(format!("shepherd-topic-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // 1. topic list --json initially empty
    let out_list_empty = run_with_buf(&root, &["topic", "list", "--json"], None, None).unwrap();
    let list_empty_val: Value = serde_json::from_str(&out_list_empty).unwrap();
    assert_eq!(list_empty_val.as_array().unwrap().len(), 0);

    // 2. topic create orders --partitions 3 --json
    let out_create = run_with_buf(
        &root,
        &["topic", "create", "orders", "--partitions", "3", "--json"],
        None,
        None,
    )
    .unwrap();
    let create_val: Value = serde_json::from_str(&out_create).unwrap();
    assert_eq!(create_val["name"], "orders");
    assert_eq!(create_val["partitions"], 3);

    // 3. topic list --json shows orders
    let out_list = run_with_buf(&root, &["topic", "list", "--json"], None, None).unwrap();
    let list_val: Value = serde_json::from_str(&out_list).unwrap();
    let topics = list_val.as_array().unwrap();
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0]["name"], "orders");
    assert_eq!(topics[0]["partitions"], 3);

    // 4. topic publish orders "create payment" --key k1 --json
    let out_pub = run_with_buf(
        &root,
        &[
            "topic",
            "publish",
            "orders",
            "create payment",
            "--key",
            "k1",
            "--json",
        ],
        None,
        None,
    )
    .unwrap();
    let pub_val: Value = serde_json::from_str(&out_pub).unwrap();
    assert_eq!(pub_val["topic"], "orders");
    assert_eq!(pub_val["content"], "create payment");
    assert_eq!(pub_val["key"], "k1");
    assert_eq!(pub_val["offset"], 0);

    // 5. topic tail orders --json
    let out_tail = run_with_buf(&root, &["topic", "tail", "orders", "--json"], None, None).unwrap();
    let tail_val: Value = serde_json::from_str(&out_tail).unwrap();
    let msgs = tail_val.as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"], "create payment");

    // 6. topic replay orders --from 0 --json
    let out_replay = run_with_buf(
        &root,
        &["topic", "replay", "orders", "--from", "0", "--json"],
        None,
        None,
    )
    .unwrap();
    let replay_val: Value = serde_json::from_str(&out_replay).unwrap();
    assert_eq!(replay_val.as_array().unwrap().len(), 1);

    // 7. topic groups orders --json
    let out_groups =
        run_with_buf(&root, &["topic", "groups", "orders", "--json"], None, None).unwrap();
    let groups_val: Value = serde_json::from_str(&out_groups).unwrap();
    assert!(groups_val.as_array().is_some());

    // 8. Human output smoke tests
    let out_human_list = run_with_buf(&root, &["topic", "list"], None, None).unwrap();
    assert!(out_human_list.contains("orders"));

    let out_human_tail = run_with_buf(&root, &["topic", "tail", "orders"], None, None).unwrap();
    assert!(out_human_tail.contains("create payment"));

    let _ = std::fs::remove_dir_all(&root);
}
