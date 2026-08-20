use super::harness::*;

#[test]
fn agent_wait_exits_immediately_when_status_already_matches() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_immediate_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let pane_id = format!("{workspace_id}:p1");

    let reported = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_immediate_2","method":"pane.report_agent","params":{{"pane_id":"{}","source":"custom:test","agent":"pi","state":"idle"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(reported["result"]["type"], "ok");

    let waited = run_cli(
        &socket_path,
        &[
            "agent",
            "wait",
            &pane_id,
            "--until",
            "idle",
            "--timeout",
            "1000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["result"]["agent"]["agent_status"], "idle");
    assert_eq!(waited_json["result"]["agent"]["agent"], "pi");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_wait_times_out_when_status_does_not_match() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_timeout_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert_eq!(created["result"]["type"], "workspace_created");
    let pane_id = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    let reported = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_timeout_2","method":"pane.report_agent","params":{{"pane_id":"{}","source":"custom:test","agent":"pi","state":"working"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(reported["result"]["type"], "ok");

    let waited = run_cli(
        &socket_path,
        &[
            "agent",
            "wait",
            pane_id,
            "--until",
            "blocked",
            "--timeout",
            "100",
        ],
    );
    assert!(!waited.status.success());
    assert!(
        String::from_utf8_lossy(&waited.stderr).contains("timed out waiting for agent status"),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn agent_wait_exits_when_done_status_matches() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(
        &fake_pi,
        "#!/bin/sh\nprintf 'starting\\n'\nsleep 4\nprintf 'Working...\\n'\nsleep 1\nprintf '\\033[2J\\033[Hdone\\n'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let herdr = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );

    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_status_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let tab_created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_status_2","method":"tab.create","params":{{"workspace_id":"{}","focus":true}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(tab_created["result"]["type"], "tab_created");

    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let start_pi = run_cli(&socket_path, &["pane", "run", &pane_id, "pi"]);
    assert!(start_pi.status.success());
    assert!(wait_until(
        Duration::from_secs(3),
        Duration::from_millis(25),
        || run_cli(&socket_path, &["agent", "get", &pane_id])
            .status
            .success()
    ));

    let waited = run_cli(
        &socket_path,
        &[
            "agent",
            "wait",
            &pane_id,
            "--until",
            "done",
            "--timeout",
            "10000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["result"]["agent"]["agent_status"], "done");
    assert_eq!(waited_json["result"]["agent"]["agent"], "pi");

    cleanup_spawned_herdr(herdr, base);
}

fn report_agent_frame(id: &str, pane_id: &str, state: &str, seq: u64) -> String {
    format!(
        r#"{{"id":"{id}","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","source":"custom:test","agent":"pi","state":"{state}","seq":{seq}}}}}"#
    )
}

/// A short idle blip that the pane immediately leaves must not complete a
/// standalone wait: the wait must re-probe and report only settled state.
/// See issue #2851.
#[test]
fn agent_wait_does_not_return_a_transient_status() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_transient_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let reported = send_request(
        &socket_path,
        &report_agent_frame("req_cli_transient_2", &pane_id, "working", 1),
    );
    assert_eq!(reported["result"]["type"], "ok");

    let wait_socket = socket_path.clone();
    let wait_pane = pane_id.clone();
    let waiter = thread::spawn(move || {
        run_cli(
            &wait_socket,
            &[
                "agent",
                "wait",
                &wait_pane,
                "--until",
                "idle",
                "--timeout",
                "2000",
            ],
        )
    });

    // Let the wait record its event cursor and enter its poll loop.
    thread::sleep(Duration::from_millis(300));

    // Blip idle and restore working. Both frames are written before either
    // response is read, so the server applies them back to back and the pane is
    // working again by the time the wait can probe.
    let mut idle_conn = UnixStream::connect(&socket_path).unwrap();
    let mut working_conn = UnixStream::connect(&socket_path).unwrap();
    let idle_frame = report_agent_frame("req_cli_transient_3", &pane_id, "idle", 2);
    let working_frame = report_agent_frame("req_cli_transient_4", &pane_id, "working", 3);
    idle_conn
        .write_all(format!("{idle_frame}\n").as_bytes())
        .unwrap();
    idle_conn.flush().unwrap();
    working_conn
        .write_all(format!("{working_frame}\n").as_bytes())
        .unwrap();
    working_conn.flush().unwrap();
    for conn in [&idle_conn, &working_conn] {
        let mut line = String::new();
        BufReader::new(conn).read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["type"], "ok");
    }

    let waited = waiter.join().unwrap();
    let stdout = String::from_utf8_lossy(&waited.stdout).to_string();
    let stderr = String::from_utf8_lossy(&waited.stderr).to_string();

    assert!(
        !waited.status.success(),
        "agent wait completed on a transient idle blip: stdout: {stdout}"
    );
    assert!(
        stderr.contains("timed out waiting for agent status"),
        "stderr: {stderr}"
    );

    let current = run_cli_json(&socket_path, &["agent", "get", &pane_id]);
    assert_eq!(current["result"]["agent"]["agent_status"], "working");

    cleanup_spawned_herdr(herdr, base);
}
