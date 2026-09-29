use super::harness::*;

#[test]
fn pane_close_only_removes_the_target_tab_when_other_tabs_exist() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let workspace_id = created_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let created_tab = run_cli(
        &socket_path,
        &["tab", "create", "--workspace", &workspace_id],
    );
    assert!(created_tab.status.success());
    let created_tab_json: serde_json::Value = serde_json::from_slice(&created_tab.stdout).unwrap();
    let second_root_pane_id = created_tab_json["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let closed = run_cli(&socket_path, &["pane", "close", &second_root_pane_id]);
    assert!(closed.status.success());
    let closed_json: serde_json::Value = serde_json::from_slice(&closed.stdout).unwrap();
    assert_eq!(closed_json["result"]["type"], "ok");

    let workspaces = run_cli(&socket_path, &["workspace", "list"]);
    assert!(workspaces.status.success());
    let workspaces_json: serde_json::Value = serde_json::from_slice(&workspaces.stdout).unwrap();
    assert_eq!(
        workspaces_json["result"]["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        workspaces_json["result"]["workspaces"][0]["workspace_id"],
        workspace_id
    );

    let tabs = run_cli(&socket_path, &["tab", "list", "--workspace", &workspace_id]);
    assert!(tabs.status.success());
    let tabs_json: serde_json::Value = serde_json::from_slice(&tabs.stdout).unwrap();
    assert_eq!(tabs_json["result"]["tabs"].as_array().unwrap().len(), 1);

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn pane_close_replaces_the_last_tab_without_removing_the_workspace() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let workspace_id = created_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let root_pane_id = created_json["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let original_tabs = run_cli(&socket_path, &["tab", "list", "--workspace", &workspace_id]);
    assert!(original_tabs.status.success());
    let original_tabs_json: serde_json::Value =
        serde_json::from_slice(&original_tabs.stdout).unwrap();
    let original_tab_id = original_tabs_json["result"]["tabs"][0]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();

    let closed = run_cli(&socket_path, &["pane", "close", &root_pane_id]);
    assert!(closed.status.success());
    let closed_json: serde_json::Value = serde_json::from_slice(&closed.stdout).unwrap();
    assert_eq!(closed_json["result"]["type"], "ok");

    let workspaces = run_cli(&socket_path, &["workspace", "list"]);
    assert!(workspaces.status.success());
    let workspaces_json: serde_json::Value = serde_json::from_slice(&workspaces.stdout).unwrap();
    assert_eq!(
        workspaces_json["result"]["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        workspaces_json["result"]["workspaces"][0]["workspace_id"],
        workspace_id
    );

    let tabs = run_cli(&socket_path, &["tab", "list", "--workspace", &workspace_id]);
    assert!(tabs.status.success());
    let tabs_json: serde_json::Value = serde_json::from_slice(&tabs.stdout).unwrap();
    assert_eq!(tabs_json["result"]["tabs"].as_array().unwrap().len(), 1);
    let replacement_tab_id = tabs_json["result"]["tabs"][0]["tab_id"].as_str().unwrap();
    assert_ne!(replacement_tab_id, original_tab_id);

    let panes = run_cli(
        &socket_path,
        &["pane", "list", "--workspace", &workspace_id],
    );
    assert!(panes.status.success());
    let panes_json: serde_json::Value = serde_json::from_slice(&panes.stdout).unwrap();
    let replacement_pane = &panes_json["result"]["panes"][0];
    assert_ne!(replacement_pane["pane_id"], root_pane_id);
    assert_eq!(replacement_pane["tab_id"], replacement_tab_id);
    assert_eq!(replacement_pane["cwd"], base.to_str().unwrap());

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn pane_run_read_and_wait_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let create = run_cli(
        &socket_path,
        &[
            "pane",
            "run",
            "1-1",
            "echo alpha && echo beta && printf 'ready\\n'",
        ],
    );
    assert!(create.status.success());

    let started = Instant::now();
    let waited = run_cli(
        &socket_path,
        &[
            "pane",
            "wait-output",
            "1-1",
            "--match",
            "ready",
            "--source",
            "recent",
            "--lines",
            "40",
            "--timeout",
            "5000",
        ],
    );
    let elapsed = started.elapsed();
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "already-matching wait took {elapsed:?}"
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["result"]["type"], "output_matched");

    let read = run_cli(
        &socket_path,
        &["pane", "read", "1-1", "--source", "recent", "--lines", "40"],
    );
    assert!(read.status.success());
    let text = String::from_utf8(read.stdout).unwrap();
    assert!(text.contains("alpha"));
    assert!(text.contains("ready"));

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn wait_output_matches_recent_unwrapped_text() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let token = "WRAP_WAIT_TEST_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789";
    let script = base.join("emit-long-token.sh");
    std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{token}'\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }

    let run = run_cli(
        &socket_path,
        &["pane", "run", "1-1", &format!("sh {}", script.display())],
    );
    assert!(run.status.success());

    let waited = run_cli(
        &socket_path,
        &[
            "pane",
            "wait-output",
            "1-1",
            "--match",
            token,
            "--source",
            "recent",
            "--lines",
            "80",
            "--timeout",
            "5000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {} stdout: {}",
        String::from_utf8_lossy(&waited.stderr),
        String::from_utf8_lossy(&waited.stdout)
    );

    let read = run_cli(
        &socket_path,
        &[
            "pane",
            "read",
            "1-1",
            "--source",
            "recent-unwrapped",
            "--lines",
            "80",
        ],
    );
    assert!(read.status.success());
    let text = String::from_utf8(read.stdout).unwrap();
    assert!(text.contains(token));

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn closing_pane_terminates_processes_inside_it() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let split = run_cli(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right"],
    );
    assert!(split.status.success());
    let split_json: serde_json::Value = serde_json::from_slice(&split.stdout).unwrap();
    let pane_id = split_json["result"]["pane"]["pane_id"].as_str().unwrap();

    let pid_file = base.join("pane-close.pid");
    let command = format!(
        "python3 -c 'import os,time,pathlib; pathlib.Path(r\"{}\").write_text(str(os.getpid())); time.sleep(1000)'",
        pid_file.display()
    );
    let ran = run_cli(&socket_path, &["pane", "run", pane_id, &command]);
    assert!(
        ran.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !pid_file.exists() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(pid_file.exists(), "pid file was not created");

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(3)).unwrap_or_else(|err| {
        panic!("failed to read pane child pid: {err}");
    });
    assert!(process_exists(pid), "child process was not running");

    let closed = run_cli(&socket_path, &["pane", "close", pane_id]);
    assert!(
        closed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "process {pid} survived pane close"
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn closing_workspace_terminates_processes_inside_it() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let pid_file = base.join("workspace-close.pid");
    let command = format!(
        "python3 -c 'import os,time,pathlib; pathlib.Path(r\"{}\").write_text(str(os.getpid())); time.sleep(1000)'",
        pid_file.display()
    );
    let ran = run_cli(&socket_path, &["pane", "run", "1-1", &command]);
    assert!(
        ran.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !pid_file.exists() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(pid_file.exists(), "pid file was not created");

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(3)).unwrap_or_else(|err| {
        panic!("failed to read pane child pid: {err}");
    });
    assert!(process_exists(pid), "child process was not running");

    let closed = run_cli(&socket_path, &["workspace", "close", "1"]);
    assert!(
        closed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "process {pid} survived workspace close"
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn workspace_ids_and_public_pane_ids_are_stable() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let ws1_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let ws1_id = ws1_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let split_12_json = run_cli_json(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right", "--no-focus"],
    );
    assert_eq!(
        split_12_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}:p2")
    );

    let split_13_json = run_cli_json(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "down", "--no-focus"],
    );
    assert_eq!(
        split_13_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}:p3")
    );

    let ws2_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", "/tmp", "--no-focus"],
    );
    let ws2_id = ws2_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ws2_id, ws1_id);

    let ws2_focus = run_cli(&socket_path, &["workspace", "focus", &ws2_id]);
    assert!(
        ws2_focus.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ws2_focus.stderr)
    );

    let ws2_split_json = run_cli_json(
        &socket_path,
        &["pane", "split", "2-1", "--direction", "right", "--no-focus"],
    );
    assert_eq!(
        ws2_split_json["result"]["pane"]["pane_id"],
        format!("{ws2_id}:p2")
    );

    let ws3_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", "/", "--no-focus"],
    );
    let ws3_id = ws3_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ws3_id, ws1_id);
    assert_ne!(ws3_id, ws2_id);

    let close_ws2 = run_cli(&socket_path, &["workspace", "close", &ws2_id]);
    assert!(
        close_ws2.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&close_ws2.stderr)
    );

    let workspaces_json = run_cli_json(&socket_path, &["workspace", "list"]);
    let ids: Vec<String> = workspaces_json["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ws| ws["workspace_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![ws1_id.clone(), ws3_id.clone()]);

    let new_ws_json = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", "/var/tmp", "--no-focus"],
    );
    let new_ws_id = new_ws_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_ws_id, ws1_id);
    assert_ne!(new_ws_id, ws2_id);
    assert_ne!(new_ws_id, ws3_id);

    let ws3_panes_json = run_cli_json(&socket_path, &["pane", "list", "--workspace", &ws3_id]);
    assert_eq!(
        ws3_panes_json["result"]["panes"][0]["pane_id"],
        format!("{ws3_id}:p1")
    );

    let close_middle = run_cli(&socket_path, &["pane", "close", &format!("{ws1_id}-2")]);
    assert!(
        close_middle.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&close_middle.stderr)
    );

    let ws1_panes_json = run_cli_json(&socket_path, &["pane", "list", "--workspace", &ws1_id]);
    let pane_ids: Vec<String> = ws1_panes_json["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| pane["pane_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        pane_ids,
        vec![format!("{ws1_id}:p1"), format!("{ws1_id}:p3")]
    );

    let closed_lookup = run_cli(&socket_path, &["pane", "get", &format!("{ws1_id}:p2")]);
    assert!(
        !closed_lookup.status.success(),
        "closed pane id should not retarget: {}",
        String::from_utf8_lossy(&closed_lookup.stdout)
    );

    let split_14_json = run_cli_json(
        &socket_path,
        &[
            "pane",
            "split",
            &format!("{ws1_id}:p1"),
            "--direction",
            "right",
            "--no-focus",
        ],
    );
    assert_eq!(
        split_14_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}:p4")
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn pane_shell_gets_herdr_socket_and_pane_env() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_env_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let env_capture = base.join("pane-env.txt");
    let ran = run_cli(
        &socket_path,
        &[
            "pane",
            "run",
            "1-1",
            &format!(
                "printf '%s\\n%s\\n' \"$HERDR_SOCKET_PATH\" \"$HERDR_PANE_ID\" > {}",
                env_capture.display()
            ),
        ],
    );
    assert!(ran.status.success());

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut text = String::new();
    while Instant::now() < deadline {
        if env_capture.exists() {
            text = fs::read_to_string(&env_capture).unwrap();
            if text.contains(&socket_path.display().to_string()) && text.contains(&pane_id) {
                break;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(env_capture.exists(), "env capture file was not created");
    assert!(
        text.contains(&socket_path.display().to_string()),
        "env file was: {text:?}"
    );
    assert!(text.contains(&pane_id), "env file was: {text:?}");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn pane_agent_reports_accept_options_before_pane() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_agent_report_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let state_report = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            "--source=custom:cli-test",
            "--agent=cli-test",
            "--state=working",
            "--message=parsing=works",
            &pane_id,
        ],
    );
    assert!(
        state_report.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&state_report.stderr)
    );
    assert!(state_report.stdout.is_empty());
    assert!(state_report.stderr.is_empty());

    let agent = run_cli_json(&socket_path, &["agent", "get", &pane_id]);
    assert_eq!(agent["result"]["agent"]["agent"], "cli-test");
    assert_eq!(agent["result"]["agent"]["agent_status"], "working");

    let session_report = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent-session",
            "--source=custom:cli-test",
            "--agent=cli-test",
            "--seq=1",
            "--agent-session-id=session=1",
            "--session-start-source=startup",
            &pane_id,
        ],
    );
    assert!(
        session_report.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&session_report.stderr)
    );
    assert!(session_report.stdout.is_empty());
    assert!(session_report.stderr.is_empty());

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn pane_read_rejects_invalid_value_with_usage_error() {
    // Invalid option values fail as CLI usage errors before any server
    // contact: exit 2 with the plain parser message, not the old
    // `Error: Custom { ... }` io::Error wrapper from main.
    let socket_path = Path::new("/tmp/herdr-cli-invalid-values-no-server.sock");

    let read = run_cli(socket_path, &["pane", "read", "w1:p1", "--source", "bogus"]);
    assert_eq!(read.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&read.stderr);
    assert!(stderr.contains("invalid read source: bogus"));
    assert!(!stderr.contains("Error: Custom"));
}

fn session_focus(socket_path: &std::path::Path) -> (String, String) {
    let snapshot = run_cli_json(socket_path, &["api", "snapshot"]);
    let snapshot = &snapshot["result"]["snapshot"];
    (
        snapshot["focused_workspace_id"]
            .as_str()
            .expect("snapshot should report a focused workspace")
            .to_string(),
        snapshot["focused_pane_id"]
            .as_str()
            .expect("snapshot should report a focused pane")
            .to_string(),
    )
}

/// Socket-API-initiated `workspace create` and `pane move` must never yank the
/// human's cursor. Focus is opt-in via `--focus`; without it the session focus
/// stays exactly where the user left it, including when the moved pane is the
/// pane the user is sitting in.
#[test]
fn api_initiated_move_and_workspace_create_do_not_steal_focus() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    // The human's workspace: first one created, so it becomes the active one.
    let human_ws = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    )["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let root_pane = format!("{human_ws}:p1");

    let mut spare_panes = Vec::new();
    for direction in ["right", "down", "right"] {
        let split = run_cli_json(
            &socket_path,
            &[
                "pane",
                "split",
                &root_pane,
                "--direction",
                direction,
                "--no-focus",
            ],
        );
        spare_panes.push(
            split["result"]["pane"]["pane_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }

    let baseline = session_focus(&socket_path);
    assert_eq!(baseline, (human_ws.clone(), root_pane.clone()));

    // 1. workspace.create without a focus flag must not switch workspaces.
    let agent_ws = run_cli_json(&socket_path, &["workspace", "create", "--cwd", "/tmp"])["result"]
        ["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(agent_ws, human_ws);
    assert_eq!(
        session_focus(&socket_path),
        baseline,
        "workspace create without --focus must leave the user's focus alone"
    );

    // 2. pane.move without a focus flag must not move focus, and must not
    //    report the freshly created tab as focused.
    let moved = run_cli_json(
        &socket_path,
        &[
            "pane",
            "move",
            &spare_panes[0],
            "--new-tab",
            "--workspace",
            &agent_ws,
        ],
    );
    assert_eq!(
        moved["result"]["move_result"]["created_tab"]["focused"],
        false
    );
    assert_eq!(
        session_focus(&socket_path),
        baseline,
        "pane move without --focus must leave the user's focus alone"
    );

    // 3. Moving the pane the user is sitting in keeps focus in the SOURCE
    //    workspace; it must never jump to the destination workspace.
    let moved_focused = run_cli_json(
        &socket_path,
        &[
            "pane",
            "move",
            &root_pane,
            "--new-tab",
            "--workspace",
            &agent_ws,
        ],
    );
    let move_result = &moved_focused["result"]["move_result"];
    let (focused_ws, focused_pane) = session_focus(&socket_path);
    assert_eq!(
        focused_ws, human_ws,
        "moving the focused pane must not switch the user to the destination workspace"
    );
    assert!(
        focused_pane.starts_with(&format!("{human_ws}:")),
        "focus should fall back inside the source workspace, got {focused_pane}"
    );
    assert_ne!(focused_pane, move_result["pane"]["pane_id"]);

    // `move_result.focused_pane_id` describes the DESTINATION layout, not the
    // session focus. Pin that documented meaning so it is not misread again.
    assert_eq!(
        move_result["focused_pane_id"],
        move_result["target_layout"]["focused_pane_id"]
    );
    assert!(
        move_result["focused_pane_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{agent_ws}:")),
        "focused_pane_id reports the destination layout"
    );
    assert_ne!(move_result["focused_pane_id"], focused_pane);

    // 4. `--focus` still focuses, for both commands.
    let opted_in = run_cli_json(
        &socket_path,
        &[
            "pane",
            "move",
            &spare_panes[1],
            "--new-tab",
            "--workspace",
            &agent_ws,
            "--focus",
        ],
    );
    let opted_in_pane = opted_in["result"]["move_result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        session_focus(&socket_path),
        (agent_ws.clone(), opted_in_pane),
        "pane move --focus must still focus the moved pane"
    );

    let focused_ws = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", "/tmp", "--focus"],
    )["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        session_focus(&socket_path).0,
        focused_ws,
        "workspace create --focus must still focus the new workspace"
    );

    cleanup_spawned_herdr(herdr, base);
}
