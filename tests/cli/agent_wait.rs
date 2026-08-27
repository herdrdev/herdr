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

#[test]
fn agent_wait_reports_the_probed_status_when_the_match_was_transient() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_transient_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert_eq!(created["result"]["type"], "workspace_created");
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let report = |request_id: &str, state: &str| {
        let reported = send_request(
            &socket_path,
            &format!(
                r#"{{"id":"{request_id}","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","source":"custom:test","agent":"pi","state":"{state}"}}}}"#
            ),
        );
        assert_eq!(reported["result"]["type"], "ok", "report: {reported}");
    };

    // The pane must not match yet, otherwise the wait resolves on its first probe.
    report("req_transient_2", "working");

    let mut divergent = None;
    for attempt in 1..=5 {
        let mut wait_stream = UnixStream::connect(&socket_path).unwrap();
        wait_stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        writeln!(
            wait_stream,
            r#"{{"id":"req_transient_wait_{attempt}","method":"agent.wait","params":{{"target":"{pane_id}","until":["idle","done"],"timeout_ms":5000}}}}"#
        )
        .unwrap();
        wait_stream.flush().unwrap();
        // Let the wait take its baseline probe and settle into its poll loop.
        thread::sleep(Duration::from_millis(300));

        // Flip back to working inside one poll window. Each report is acknowledged
        // before the next is sent, so the pane really ends up working.
        report(&format!("req_transient_flip_{attempt}_settled"), "idle");
        report(&format!("req_transient_flip_{attempt}_working"), "working");

        let mut line = String::new();
        BufReader::new(wait_stream)
            .read_line(&mut line)
            .expect("agent.wait did not answer");
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            response.get("error").is_none(),
            "agent.wait failed: {response}"
        );
        let status = response["result"]["agent"]["agent_status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        match response["result"]["matched_transient_status"].as_str() {
            Some(transient) => {
                divergent = Some((status, transient.to_string()));
                break;
            }
            // The poll landed between the two reports, so the pane really held the
            // matched status when the reply was built. Flip again.
            None => assert!(
                status == "idle" || status == "done",
                "settled reply must report the matched status: {response}"
            ),
        }
    }

    let (status, transient) = divergent.expect(
        "agent.wait never reported a transient match; the reply still claims a status the pane had already left",
    );
    assert_eq!(status, "working");
    assert!(
        transient == "idle" || transient == "done",
        "transient status: {transient}"
    );

    cleanup_spawned_herdr(herdr, base);
}
