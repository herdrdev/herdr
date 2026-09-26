use super::harness::*;

fn run_claude_hook(action: &str, hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/claude/herdr-agent-state.sh",
        &[action],
        hook_input,
    )
}

fn run_codex_hook(action: &str, hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/codex/herdr-agent-state.sh",
        &[action],
        hook_input,
    )
}

fn run_copilot_hook(hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/copilot/herdr-agent-state.sh",
        &[],
        hook_input,
    )
}

fn run_devin_hook(
    action: &str,
    hook_input: &str,
    envs: &[(&str, &str)],
) -> Option<serde_json::Value> {
    run_shell_hook_with_env(
        "src/integration/assets/devin/herdr-agent-state.sh",
        &[action],
        hook_input,
        envs,
    )
}

fn run_grok_hook(hook_input: &str, envs: &[(&str, &str)]) -> Option<serde_json::Value> {
    run_shell_hook_with_env(
        "src/integration/assets/grok/herdr-agent-state.sh",
        &["session"],
        hook_input,
        envs,
    )
}

fn run_shell_hook(asset_path: &str, args: &[&str], hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook_with_env(asset_path, args, hook_input, &[])
}

fn run_shell_hook_with_env(
    asset_path: &str,
    args: &[&str],
    hook_input: &str,
    envs: &[(&str, &str)],
) -> Option<serde_json::Value> {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut line = String::new();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    reader.read_line(&mut line).unwrap();
                    let _ = stream.write_all(br#"{"id":"test","result":{"type":"ok"}}"#);
                    let _ = stream.write_all(b"\n");
                    let _ = stream.flush();
                    return Some(line);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        }
        None
    });

    let hook_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(asset_path);
    let mut command = Command::new("bash");
    command
        .arg(hook_path)
        .args(args)
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", &socket_path)
        .env("HERDR_PANE_ID", "p_test")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CURSOR_VERSION")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(hook_input.as_bytes()).unwrap();
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "hook failed: status={:?} stderr={} stdout={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let request = server.join().unwrap();
    cleanup_test_base(&base);
    request.map(|line| serde_json::from_str(&line).unwrap())
}

#[test]
fn claude_hook_ignores_state_actions() {
    let subagent_input = r#"{"hook_event_name":"Notification","agent_id":"agent-abc123","agent_type":"Explore","notification_type":"permission_prompt"}"#;

    assert!(run_claude_hook("working", subagent_input).is_none());
    assert!(run_claude_hook("blocked", subagent_input).is_none());
}

#[test]
fn claude_hook_ignores_subagent_completion_reports() {
    let subagent_input =
        r#"{"hook_event_name":"SubagentStop","agent_id":"agent-abc123","agent_type":"Explore"}"#;

    assert!(run_claude_hook("working", subagent_input).is_none());
    assert!(run_claude_hook("idle", subagent_input).is_none());
    assert!(run_claude_hook("release", subagent_input).is_none());
}

#[test]
fn claude_hook_keeps_parent_agent_type_only_blocked() {
    let request = run_claude_hook(
        "blocked",
        r#"{"hook_event_name":"PermissionRequest","agent_type":"Explore"}"#,
    );

    assert!(request.is_none());
}

#[test]
fn claude_hook_reports_session_id_from_stdin() {
    let request = run_claude_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"claude-session"}"#,
    )
    .expect("session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "claude-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn claude_hook_ignores_cursor_compatibility_payloads() {
    assert!(run_claude_hook(
        "session",
        r#"{"hook_event_name":"sessionStart","session_id":"cursor-session"}"#,
    )
    .is_none());

    assert!(run_claude_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"cursor-session","cursor_version":"2026.08.11-e8db854"}"#,
    )
    .is_none());

    for cursor_version in ["2026.08.11-e8db854", ""] {
        assert!(run_shell_hook_with_env(
            "src/integration/assets/claude/herdr-agent-state.sh",
            &["session"],
            r#"{"hook_event_name":"SessionStart","session_id":"cursor-session"}"#,
            &[("CURSOR_VERSION", cursor_version)],
        )
        .is_none());
    }
}

#[test]
fn codex_hook_reports_persisted_root_session_and_ignores_ephemeral_or_nested_sessions() {
    let request = run_codex_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"codex-session","transcript_path":"/tmp/codex-session.jsonl"}"#,
    )
    .expect("codex hook should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "codex-session");
    assert!(request["params"].get("state").is_none());

    let matching_request = run_shell_hook_with_env(
        "src/integration/assets/codex/herdr-agent-state.sh",
        &["session"],
        r#"{"hook_event_name":"SessionStart","session_id":"codex-session","transcript_path":"/tmp/codex-session.jsonl"}"#,
        &[("CODEX_THREAD_ID", "codex-session")],
    )
    .expect("matching inherited session should still report");
    assert_eq!(
        matching_request["params"]["agent_session_id"],
        "codex-session"
    );

    assert!(run_codex_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"side-session","transcript_path":null}"#,
    )
    .is_none());

    assert!(run_shell_hook_with_env(
        "src/integration/assets/codex/herdr-agent-state.sh",
        &["session"],
        r#"{"hook_event_name":"SessionStart","session_id":"nested-session","transcript_path":"/tmp/nested-session.jsonl"}"#,
        &[("CODEX_THREAD_ID", "parent-session")],
    )
    .is_none());
}

#[test]
fn copilot_hook_reports_session_id_from_stdin() {
    let request = run_copilot_hook(
        r#"{"hook_event_name":"SessionStart","session_id":"copilot-session","source":"resume"}"#,
    )
    .expect("copilot session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent"], "copilot");
    assert_eq!(request["params"]["agent_session_id"], "copilot-session");
    assert!(request["params"].get("state").is_none());

    let camel = run_copilot_hook(
        r#"{"sessionId":"copilot-camel-session","source":"new","initialPrompt":"run tests"}"#,
    )
    .expect("copilot camelCase session start should report session identity");

    assert_eq!(camel["method"], "pane.report_agent_session");
    assert_eq!(camel["params"]["agent_session_id"], "copilot-camel-session");
    assert!(camel["params"].get("state").is_none());
}

#[test]
fn grok_hook_reports_new_session_source() {
    let request = run_grok_hook(
        r#"{"hook_event_name":"session_start","source":"new","session_id":"new-session"}"#,
        &[("GROK_SESSION_ID", "new-session")],
    )
    .expect("grok session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "new-session");
    assert_eq!(request["params"]["session_start_source"], "new");
}

#[test]
fn copilot_hook_does_not_report_lifecycle_state() {
    for payload in [
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"copilot-session","prompt":"run tests"}"#,
        r#"{"hook_event_name":"PreToolUse","session_id":"copilot-session","tool_name":"ask_user"}"#,
        r#"{"hook_event_name":"notification","session_id":"copilot-session","notification_type":"permission_prompt"}"#,
        r#"{"hook_event_name":"agentStop","session_id":"copilot-session","stop_reason":"end_turn"}"#,
        r#"{"hook_event_name":"SessionEnd","session_id":"copilot-session","reason":"user_exit"}"#,
    ] {
        assert!(
            run_copilot_hook(payload).is_none(),
            "copilot session-only hook should ignore lifecycle payload {payload}"
        );
    }
}

#[test]
fn devin_hook_ignores_prompt_session_list_fallback() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"run tests"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"older-session","working_directory":"/tmp/other"},{"id":"devin-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    );

    assert!(request.is_none());
}

#[test]
fn devin_hook_reports_session_id_from_stdin_without_state() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"devin-session","source":"startup"}"#,
        &[("HERDR_DEVIN_LIST_JSON", r#"[{"id":"older-session"}]"#)],
    )
    .expect("devin session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent"], "devin");
    assert_eq!(request["params"]["agent_session_id"], "devin-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn devin_hook_prefers_hook_session_id_over_list() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"PreToolUse","sessionId":"fresh-session","tool_name":"exec"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"older-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    )
    .expect("devin tool hook should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "fresh-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn devin_hook_reports_tool_session_from_list_without_state() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"PreToolUse","tool_name":"exec"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"older-session","working_directory":"/tmp/other"},{"id":"devin-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    )
    .expect("devin tool hook should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent"], "devin");
    assert_eq!(request["params"]["agent_session_id"], "devin-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn devin_hook_ignores_startup_session_list_fallback() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"stale-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    );

    assert!(request.is_none());
}

#[test]
fn devin_hook_ignores_non_matching_session_list_entries() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"PreToolUse","tool_name":"exec"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"other-session","working_directory":"/tmp/other"}]"#,
            ),
        ],
    );

    assert!(request.is_none());
}

fn run_kiro_hook(hook_input: &str) -> Option<serde_json::Value> {
    run_kiro_hook_with_binary(
        hook_input,
        Path::new(env!("CARGO_BIN_EXE_herdr")),
        Duration::from_secs(2),
        &[],
    )
}

fn run_kiro_hook_with_binary(
    hook_input: &str,
    herdr_bin: &Path,
    server_timeout: Duration,
    envs: &[(&str, &str)],
) -> Option<serde_json::Value> {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let api_socket = base.join("herdr.sock");
    let listener = UnixListener::bind(&api_socket).unwrap();

    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + server_timeout;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut line = String::new();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    reader.read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    if request["method"] == "ping" {
                        write_fake_pong(&mut stream, &request, "kiro-hook-test", CURRENT_PROTOCOL);
                        continue;
                    }
                    writeln!(
                        stream,
                        "{}",
                        serde_json::json!({
                            "id": request["id"],
                            "result": {"type": "ok"},
                        })
                    )
                    .unwrap();
                    stream.flush().unwrap();
                    return Some(request);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        }
        None
    });

    let hook_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/integration/assets/kiro/herdr-agent-state.sh");
    let mut command = Command::new("bash");
    command
        .arg(hook_path)
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", &api_socket)
        .env("HERDR_PANE_ID", "p_test")
        .env("HERDR_BIN_PATH", herdr_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(hook_input.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "kiro hook failed: status={:?} stderr={} stdout={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let request = server.join().unwrap();
    cleanup_test_base(&base);
    request
}

fn kiro_session_change_payload(location: &str, session_id: &str, transition_seq: u64) -> String {
    serde_json::json!({
        "hook_event_name": "SessionChange",
        "session_id": session_id,
        "cwd": "/tmp/kiro-hook-test",
        "session_location": location,
        "client_pid": std::process::id(),
        "transition_seq": transition_seq,
    })
    .to_string()
}

#[test]
fn kiro_hook_reports_local_leaf_session_through_real_cli_command() {
    let request = run_kiro_hook(&kiro_session_change_payload("local", "leaf-local", 42))
        .expect("local SessionChange should report through the Herdr CLI");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["pane_id"], "p_test");
    assert_eq!(request["params"]["source"], "herdr:kiro-v3");
    assert_eq!(request["params"]["agent"], "kiro");
    assert_eq!(request["params"]["seq"], 42);
    assert_eq!(request["params"]["agent_session_id"], "leaf-local");
    assert_eq!(request["params"]["session_start_source"], "select");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn kiro_install_requires_python3_before_writing_files() {
    let base = unique_test_dir();
    let kiro_dir = base.join("kiro");
    let empty_bin = base.join("empty-bin");
    fs::create_dir_all(&kiro_dir).unwrap();
    fs::create_dir_all(&empty_bin).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["integration", "install", "kiro"])
        .env("KIRO_CONFIG_DIR", &kiro_dir)
        .env("PATH", &empty_bin)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("python3 is required for the kiro integration"));
    assert!(message.contains("install Python 3 and retry"));
    assert!(!kiro_dir.join("hooks").exists());
    cleanup_test_base(&base);
}

#[test]
fn kiro_hook_allows_delayed_report_beyond_one_second() {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let delayed_herdr = base.join("delayed-herdr");
    fs::write(
        &delayed_herdr,
        "#!/bin/sh\nsleep 2\nexec \"$HERDR_TEST_REAL_BIN\" \"$@\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&delayed_herdr).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&delayed_herdr, permissions).unwrap();

    let request = run_kiro_hook_with_binary(
        &kiro_session_change_payload("local", "delayed-leaf", 46),
        &delayed_herdr,
        Duration::from_secs(3),
        &[("HERDR_TEST_REAL_BIN", env!("CARGO_BIN_EXE_herdr"))],
    )
    .expect("a report that starts after one second should reach Herdr");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["source"], "herdr:kiro-v3");
    assert_eq!(request["params"]["agent_session_id"], "delayed-leaf");

    cleanup_test_base(&base);
}

#[test]
fn kiro_hook_releases_remote_identity_through_real_cli_command() {
    let request = run_kiro_hook(&kiro_session_change_payload("remote", "leaf-remote", 43))
        .expect("remote SessionChange should release local identity through the Herdr CLI");

    assert_eq!(request["method"], "pane.release_agent");
    assert_eq!(request["params"]["pane_id"], "p_test");
    assert_eq!(request["params"]["source"], "herdr:kiro-v3");
    assert_eq!(request["params"]["agent"], "kiro");
    assert_eq!(request["params"]["seq"], 43);
    assert!(request["params"].get("agent_session_id").is_none());
    assert!(request["params"].get("state").is_none());
}

#[test]
fn kiro_hook_ignores_malformed_legacy_and_unknown_payloads() {
    let valid = serde_json::from_str::<serde_json::Value>(&kiro_session_change_payload(
        "local", "leaf", 44,
    ))
    .unwrap();
    let mut missing_session = valid.clone();
    missing_session
        .as_object_mut()
        .unwrap()
        .remove("session_id");
    let mut zero_seq = valid.clone();
    zero_seq["transition_seq"] = serde_json::json!(0);
    let mut dead_client = valid.clone();
    dead_client["client_pid"] = serde_json::json!(0);
    let mut exited_child = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
    let exited_pid = exited_child.id();
    exited_child.wait().unwrap();
    let mut exited_client = valid.clone();
    exited_client["client_pid"] = serde_json::json!(exited_pid);
    let mut unknown_location = valid.clone();
    unknown_location["session_location"] = serde_json::json!("cloud");

    for payload in [
        serde_json::json!({}),
        missing_session,
        zero_seq,
        dead_client,
        exited_client,
        unknown_location,
        serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "leaf",
            "cwd": "/tmp/kiro-hook-test",
            "session_location": "local",
            "client_pid": std::process::id(),
            "transition_seq": 44,
        }),
        serde_json::json!({
            "hook_event_name": "SessionChangeV2",
            "session_id": "leaf",
            "cwd": "/tmp/kiro-hook-test",
            "session_location": "local",
            "client_pid": std::process::id(),
            "transition_seq": 44,
        }),
    ] {
        assert!(
            run_kiro_hook(&payload.to_string()).is_none(),
            "Kiro hook should ignore payload {payload}"
        );
    }
}

#[test]
fn kiro_hook_passes_malicious_session_id_as_a_single_argument() {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let capture = base.join("args.bin");
    let marker = base.join("executed");
    let fake_herdr = base.join("herdr");
    fs::write(
        &fake_herdr,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\n",
            capture.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_herdr).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_herdr, permissions).unwrap();

    let session_id = format!("leaf; touch {} #", marker.display());
    let hook_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/integration/assets/kiro/herdr-agent-state.sh");
    let mut child = Command::new("bash")
        .arg(hook_path)
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", base.join("herdr.sock"))
        .env("HERDR_PANE_ID", "p_test")
        .env("HERDR_BIN_PATH", &fake_herdr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(kiro_session_change_payload("local", &session_id, 45).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let args = fs::read(&capture)
        .unwrap()
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(args[0..3], ["pane", "report-agent-session", "p_test"]);
    assert!(args.contains(&session_id));
    assert!(!marker.exists());

    cleanup_test_base(&base);
}
