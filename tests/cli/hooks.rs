use super::harness::*;

// Shell integrations belong in `shell_hooks_conform_to_session_report_contract`.
// The table exercises the asset against a real Unix socket and checks Herdr's
// stable session-report contract. Keep adapter-specific edge cases as separate
// tests below, and add a shared-socket case when an agent can have concurrent
// panes (as Jcode does).
#[derive(Clone, Copy)]
struct ShellHookInvocation<'a> {
    asset_path: &'a str,
    args: &'a [&'a str],
    hook_input: &'a str,
    envs: &'a [(&'a str, &'a str)],
    pane_id: &'a str,
}

struct FakeHookSocket {
    base: PathBuf,
    socket_path: PathBuf,
    server: thread::JoinHandle<Vec<serde_json::Value>>,
}

impl FakeHookSocket {
    fn start(expected_requests: usize) -> Self {
        let base = unique_test_dir();
        fs::create_dir_all(&base).unwrap();
        let socket_path = base.join("herdr.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(700);
            let mut requests = Vec::with_capacity(expected_requests);
            while requests.len() < expected_requests && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut line = String::new();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        reader.read_line(&mut line).unwrap();
                        let _ = stream.write_all(br#"{"id":"test","result":{"type":"ok"}}"#);
                        let _ = stream.write_all(b"\n");
                        let _ = stream.flush();
                        requests.push(serde_json::from_str(&line).unwrap());
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("accept failed: {err}"),
                }
            }
            requests
        });

        Self {
            base,
            socket_path,
            server,
        }
    }

    fn finish(self) -> Vec<serde_json::Value> {
        let requests = self.server.join().unwrap();
        cleanup_test_base(&self.base);
        requests
    }
}

fn invoke_shell_hook(invocation: &ShellHookInvocation<'_>, socket_path: &Path) {
    let hook_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(invocation.asset_path);
    let mut command = Command::new("bash");
    command
        .arg(hook_path)
        .args(invocation.args)
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", socket_path)
        .env("HERDR_PANE_ID", invocation.pane_id)
        .env_remove("CODEX_THREAD_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in invocation.envs {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(invocation.hook_input.as_bytes()).unwrap();
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "hook failed: asset={} status={:?} stderr={} stdout={}",
        invocation.asset_path,
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

fn run_shell_hooks(invocations: &[ShellHookInvocation<'_>]) -> Vec<serde_json::Value> {
    let socket = FakeHookSocket::start(invocations.len());
    for invocation in invocations {
        invoke_shell_hook(invocation, &socket.socket_path);
    }
    socket.finish()
}

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

fn run_shell_hook(asset_path: &str, args: &[&str], hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook_with_env(asset_path, args, hook_input, &[])
}

fn run_shell_hook_with_env(
    asset_path: &str,
    args: &[&str],
    hook_input: &str,
    envs: &[(&str, &str)],
) -> Option<serde_json::Value> {
    let socket = FakeHookSocket::start(1);
    invoke_shell_hook(
        &ShellHookInvocation {
            asset_path,
            args,
            hook_input,
            envs,
            pane_id: "p_test",
        },
        &socket.socket_path,
    );
    socket.finish().into_iter().next()
}

#[test]
fn shell_hooks_conform_to_session_report_contract() {
    struct Case<'a> {
        name: &'a str,
        invocation: ShellHookInvocation<'a>,
        agent: &'a str,
        session_id: &'a str,
    }

    let cases = [
        Case {
            name: "claude",
            invocation: ShellHookInvocation {
                asset_path: "src/integration/assets/claude/herdr-agent-state.sh",
                args: &["session"],
                hook_input: r#"{"hook_event_name":"SessionStart","session_id":"claude-session"}"#,
                envs: &[],
                pane_id: "p_claude",
            },
            agent: "claude",
            session_id: "claude-session",
        },
        Case {
            name: "codex",
            invocation: ShellHookInvocation {
                asset_path: "src/integration/assets/codex/herdr-agent-state.sh",
                args: &["session"],
                hook_input: r#"{"hook_event_name":"SessionStart","session_id":"codex-session","transcript_path":"/tmp/codex-session.jsonl"}"#,
                envs: &[],
                pane_id: "p_codex",
            },
            agent: "codex",
            session_id: "codex-session",
        },
        Case {
            name: "copilot",
            invocation: ShellHookInvocation {
                asset_path: "src/integration/assets/copilot/herdr-agent-state.sh",
                args: &[],
                hook_input: r#"{"hook_event_name":"SessionStart","session_id":"copilot-session"}"#,
                envs: &[],
                pane_id: "p_copilot",
            },
            agent: "copilot",
            session_id: "copilot-session",
        },
        Case {
            name: "devin",
            invocation: ShellHookInvocation {
                asset_path: "src/integration/assets/devin/herdr-agent-state.sh",
                args: &["session"],
                hook_input: r#"{"hook_event_name":"SessionStart","session_id":"devin-session"}"#,
                envs: &[],
                pane_id: "p_devin",
            },
            agent: "devin",
            session_id: "devin-session",
        },
        Case {
            name: "jcode",
            invocation: ShellHookInvocation {
                asset_path: "src/integration/assets/jcode/herdr-agent-state.sh",
                args: &[],
                hook_input: "",
                envs: &[("JCODE_HOOK_SESSION_ID", "jcode-session")],
                pane_id: "p_jcode",
            },
            agent: "jcode",
            session_id: "jcode-session",
        },
    ];

    for case in cases {
        let mut requests = run_shell_hooks(&[case.invocation]);
        let request = requests
            .pop()
            .unwrap_or_else(|| panic!("{} hook did not report a session", case.name));
        assert_eq!(
            request["method"], "pane.report_agent_session",
            "{}",
            case.name
        );
        assert_eq!(request["params"]["agent"], case.agent, "{}", case.name);
        assert_eq!(
            request["params"]["pane_id"], case.invocation.pane_id,
            "{}",
            case.name
        );
        assert_eq!(
            request["params"]["agent_session_id"], case.session_id,
            "{}",
            case.name
        );
        assert!(
            request["params"].get("state").is_none(),
            "{} session report included lifecycle state",
            case.name
        );
    }
}

#[test]
fn jcode_hook_maps_two_panes_to_distinct_sessions_on_one_socket() {
    let invocations = [
        ShellHookInvocation {
            asset_path: "src/integration/assets/jcode/herdr-agent-state.sh",
            args: &[],
            hook_input: "",
            envs: &[("JCODE_HOOK_SESSION_ID", "jcode-session-one")],
            pane_id: "p_jcode_one",
        },
        ShellHookInvocation {
            asset_path: "src/integration/assets/jcode/herdr-agent-state.sh",
            args: &[],
            hook_input: "",
            envs: &[("JCODE_HOOK_SESSION_ID", "jcode-session-two")],
            pane_id: "p_jcode_two",
        },
    ];

    let requests = run_shell_hooks(&invocations);
    let mappings: std::collections::HashMap<_, _> = requests
        .iter()
        .map(|request| {
            (
                request["params"]["pane_id"].as_str().unwrap(),
                request["params"]["agent_session_id"].as_str().unwrap(),
            )
        })
        .collect();

    assert_eq!(mappings.len(), 2);
    assert_eq!(mappings.get("p_jcode_one"), Some(&"jcode-session-one"));
    assert_eq!(mappings.get("p_jcode_two"), Some(&"jcode-session-two"));
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
